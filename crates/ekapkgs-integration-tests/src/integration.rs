//! Integration tests for ekapkgs-cli.
//!
//! These tests build and run the actual server binary, then exercise the HTTP
//! endpoints. They require `cargo build` to have been run first (or they build
//! inline via `cargo_bin`).

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use tempfile::TempDir;

/// Find the built binary in the target directory.
fn cargo_bin(name: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop(); // crates/
    path.pop(); // project root
    path.push("target");
    path.push("debug");
    path.push(name);
    path
}

/// Set up a filesystem cache directory with a signing key and start the server.
struct TestServer {
    child: Child,
    port: u16,
    cache_dir: TempDir,
    _token_dir: TempDir,
    _signing_key_dir: TempDir,
}

impl TestServer {
    fn start() -> Self {
        Self::start_with_tokens(&[])
    }

    fn start_with_tokens(token_names: &[&str]) -> Self {
        let cache_dir = TempDir::new().expect("create cache dir");
        let signing_key_dir = TempDir::new().expect("create key dir");
        let token_dir = TempDir::new().expect("create token dir");

        // Create nar/ subdirectory.
        std::fs::create_dir_all(cache_dir.path().join("nar")).unwrap();

        // Generate a signing key in nix format: name:base64(secret+public).
        let secret = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
        let public = secret.verifying_key();
        let mut key_bytes = Vec::new();
        key_bytes.extend_from_slice(secret.as_bytes());
        key_bytes.extend_from_slice(public.as_bytes());
        let key_b64 = data_encoding::BASE64.encode(&key_bytes);
        let key_path = signing_key_dir.path().join("cache-key.sec");
        std::fs::write(&key_path, format!("test-cache-1:{key_b64}\n")).unwrap();

        // Create tokens if requested.
        let mut write_tokens = Vec::new();
        if !token_names.is_empty() {
            let token_store_path = token_dir.path().join("tokens.json");

            // Build a tokens.json manually.
            let mut tokens = Vec::new();
            for name in token_names {
                let token = format!("test_token_{name}");
                write_tokens.push(token.clone());
                tokens.push(serde_json::json!({
                    "name": name,
                    "token": token,
                    "permissions": { "read": true, "write": true },
                    "created_at": 0
                }));
            }
            let store = serde_json::json!({ "tokens": tokens });
            std::fs::write(&token_store_path, store.to_string()).unwrap();

            // Also write a config.toml that points to this token store.
            // The server looks for tokens.json next to the config file.
        }

        // Write a config file.
        let config_path = token_dir.path().join("config.toml");
        let mut config = format!(
            r#"
[server]
bind = "127.0.0.1:0"

[storage]
backend = "filesystem"
path = "{}"

[signing]
secret_key_file = "{}"
"#,
            cache_dir.path().display(),
            key_path.display(),
        );

        if !write_tokens.is_empty() {
            config.push_str("\n[auth]\n");
            let tokens_str: Vec<String> = write_tokens.iter().map(|t| format!("\"{t}\"")).collect();
            config.push_str(&format!("write_tokens = [{}]\n", tokens_str.join(", ")));
        }

        std::fs::write(&config_path, &config).unwrap();

        // Find a free port.
        let port = find_free_port();

        let bin = cargo_bin("ekapkgs-serve");
        let child = Command::new(&bin)
            .args(["--config", config_path.to_str().unwrap()])
            .args(["--bind", &format!("127.0.0.1:{port}")])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| {
                panic!(
                    "Failed to start server binary at {}: {e}\nRun `cargo build` first.",
                    bin.display()
                )
            });

        // Give the server a moment to start.
        std::thread::sleep(std::time::Duration::from_millis(500));

        Self {
            child,
            port,
            cache_dir,
            _token_dir: token_dir,
            _signing_key_dir: signing_key_dir,
        }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn start_castore() -> Self {
        Self::start_castore_with_tokens(&[])
    }

    fn start_castore_with_tokens(token_names: &[&str]) -> Self {
        let cache_dir = TempDir::new().expect("create cache dir");
        let signing_key_dir = TempDir::new().expect("create key dir");
        let token_dir = TempDir::new().expect("create token dir");

        // Generate a signing key in nix format: name:base64(secret+public).
        let secret = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
        let public = secret.verifying_key();
        let mut key_bytes = Vec::new();
        key_bytes.extend_from_slice(secret.as_bytes());
        key_bytes.extend_from_slice(public.as_bytes());
        let key_b64 = data_encoding::BASE64.encode(&key_bytes);
        let key_path = signing_key_dir.path().join("cache-key.sec");
        std::fs::write(&key_path, format!("test-cache-1:{key_b64}\n")).unwrap();

        // Create tokens if requested.
        let mut write_tokens = Vec::new();
        for name in token_names {
            write_tokens.push(format!("test_token_{name}"));
        }

        // Write a config file with castore backend.
        let config_path = token_dir.path().join("config.toml");
        let mut config = format!(
            r#"
[server]
bind = "127.0.0.1:0"

[storage]
backend = "castore"
path = "{}"

[signing]
secret_key_file = "{}"
"#,
            cache_dir.path().display(),
            key_path.display(),
        );

        if !write_tokens.is_empty() {
            config.push_str("\n[auth]\n");
            let tokens_str: Vec<String> = write_tokens.iter().map(|t| format!("\"{t}\"")).collect();
            config.push_str(&format!("write_tokens = [{}]\n", tokens_str.join(", ")));
        }

        std::fs::write(&config_path, &config).unwrap();

        let port = find_free_port();

        let bin = cargo_bin("ekapkgs-serve");
        let child = Command::new(&bin)
            .args(["--config", config_path.to_str().unwrap()])
            .args(["--bind", &format!("127.0.0.1:{port}")])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| {
                panic!(
                    "Failed to start server binary at {}: {e}\nRun `cargo build` first.",
                    bin.display()
                )
            });

        std::thread::sleep(std::time::Duration::from_millis(500));

        Self {
            child,
            port,
            cache_dir,
            _token_dir: token_dir,
            _signing_key_dir: signing_key_dir,
        }
    }

    fn write_narinfo(&self, hash: &str, narinfo: &str) {
        let path = self.cache_dir.path().join(format!("{hash}.narinfo"));
        std::fs::write(path, narinfo).unwrap();
    }

    fn write_nar(&self, filename: &str, data: &[u8]) {
        let path = self.cache_dir.path().join("nar").join(filename);
        std::fs::write(path, data).unwrap();
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn find_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

// ===== Tests =====

#[tokio::test]
async fn test_nix_cache_info() {
    let server = TestServer::start();
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/nix-cache-info", server.base_url()))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("StoreDir: /nix/store"));
    assert!(body.contains("WantMassQuery: 1"));
}

#[tokio::test]
async fn test_narinfo_get_existing() {
    let server = TestServer::start();

    // Write a narinfo directly to the cache.
    server.write_narinfo(
        "abc123",
        "StorePath: /nix/store/abc123-hello-1.0\nURL: nar/abc123.nar\nCompression: none\nNarHash: \
         sha256:deadbeef\nNarSize: 100\n",
    );

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/abc123.narinfo", server.base_url()))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("StorePath: /nix/store/abc123-hello-1.0"));
    assert!(body.contains("NarHash: sha256:deadbeef"));
    // Should have been re-signed by the server.
    assert!(body.contains("Sig: test-cache-1:"));
}

#[tokio::test]
async fn test_narinfo_get_missing() {
    let server = TestServer::start();
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/nonexistent.narinfo", server.base_url()))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_nar_get() {
    let server = TestServer::start();

    let nar_data = b"fake-nar-content-for-testing";
    server.write_nar("abc123.nar", nar_data);

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/nar/abc123.nar", server.base_url()))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.as_ref(), nar_data);
}

#[tokio::test]
async fn test_nar_get_missing() {
    let server = TestServer::start();
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/nar/nonexistent.nar", server.base_url()))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_push_narinfo_requires_auth() {
    let server = TestServer::start_with_tokens(&["ci"]);
    let client = reqwest::Client::new();

    let narinfo = "StorePath: /nix/store/xyz789-pkg-1.0\nURL: nar/xyz789.nar\nCompression: \
                   none\nNarHash: sha256:aabbccdd\nNarSize: 200\n";

    // Without token — should fail.
    let resp = client
        .put(format!("{}/xyz789.narinfo", server.base_url()))
        .body(narinfo)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // With wrong token — should fail.
    let resp = client
        .put(format!("{}/xyz789.narinfo", server.base_url()))
        .header("Authorization", "Bearer wrong_token")
        .body(narinfo)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // With correct token — should succeed.
    let resp = client
        .put(format!("{}/xyz789.narinfo", server.base_url()))
        .header("Authorization", "Bearer test_token_ci")
        .body(narinfo)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Verify it's now readable.
    let resp = client
        .get(format!("{}/xyz789.narinfo", server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("StorePath: /nix/store/xyz789-pkg-1.0"));
}

#[tokio::test]
async fn test_push_nar_and_narinfo_e2e() {
    let server = TestServer::start_with_tokens(&["writer"]);
    let client = reqwest::Client::new();
    let base = server.base_url();

    let nar_data = b"test-nar-binary-content";
    let narinfo = "StorePath: /nix/store/aaa111-test-1.0\nURL: nar/aaa111.nar\nCompression: \
                   none\nNarHash: sha256:112233\nNarSize: 50\nReferences: aaa111-test-1.0\n";

    // Push NAR.
    let resp = client
        .put(format!("{base}/nar/aaa111.nar"))
        .header("Authorization", "Bearer test_token_writer")
        .body(nar_data.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Push narinfo.
    let resp = client
        .put(format!("{base}/aaa111.narinfo"))
        .header("Authorization", "Bearer test_token_writer")
        .body(narinfo)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Verify both are readable.
    let resp = client
        .get(format!("{base}/aaa111.narinfo"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("NarHash: sha256:112233"));
    assert!(body.contains("Sig: test-cache-1:"));

    let resp = client
        .get(format!("{base}/nar/aaa111.nar"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.bytes().await.unwrap().as_ref(), nar_data);
}

#[tokio::test]
async fn test_push_rejected_without_auth_config() {
    // Server with no tokens configured — push should be rejected.
    let server = TestServer::start();
    let client = reqwest::Client::new();

    let resp = client
        .put(format!("{}/test.narinfo", server.base_url()))
        .body("StorePath: /nix/store/x\nURL: nar/x.nar\nNarHash: sha256:a\nNarSize: 1\n")
        .send()
        .await
        .unwrap();

    // Should be 403 (push not enabled) since no tokens are configured.
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn test_head_narinfo() {
    let server = TestServer::start();
    server.write_narinfo(
        "head123",
        "StorePath: /nix/store/head123-pkg-1.0\nURL: nar/head123.nar\nNarHash: \
         sha256:aabb\nNarSize: 10\n",
    );

    let client = reqwest::Client::new();

    // HEAD existing — 200.
    let resp = client
        .head(format!("{}/head123.narinfo", server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // HEAD missing — 404.
    let resp = client
        .head(format!("{}/missing.narinfo", server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_multiple_tokens() {
    let server = TestServer::start_with_tokens(&["alice", "bob"]);
    let client = reqwest::Client::new();

    let narinfo =
        "StorePath: /nix/store/multi-1.0\nURL: nar/multi.nar\nNarHash: sha256:ff\nNarSize: 1\n";

    // Alice's token works.
    let resp = client
        .put(format!("{}/multi.narinfo", server.base_url()))
        .header("Authorization", "Bearer test_token_alice")
        .body(narinfo)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Bob's token also works.
    let resp = client
        .put(format!("{}/multi2.narinfo", server.base_url()))
        .header("Authorization", "Bearer test_token_bob")
        .body(narinfo)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

// ===== CAS integration tests =====

/// Build a minimal valid NAR for testing (single regular file).
fn build_test_nar(content: &[u8]) -> Vec<u8> {
    // NAR format: str("nix-archive-1") str("(") str("type") str("regular")
    //             str("contents") str(data) str(")")
    let mut buf = Vec::new();
    nar_write_str(&mut buf, "nix-archive-1");
    nar_write_str(&mut buf, "(");
    nar_write_str(&mut buf, "type");
    nar_write_str(&mut buf, "regular");
    nar_write_str(&mut buf, "contents");
    nar_write_bytes(&mut buf, content);
    nar_write_str(&mut buf, ")");
    buf
}

fn nar_write_str(buf: &mut Vec<u8>, s: &str) {
    nar_write_bytes(buf, s.as_bytes());
}

fn nar_write_bytes(buf: &mut Vec<u8>, data: &[u8]) {
    let len = data.len() as u64;
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(data);
    let pad = (8 - (data.len() % 8)) % 8;
    buf.extend(std::iter::repeat_n(0u8, pad));
}

#[tokio::test]
async fn test_castore_nix_cache_info() {
    let server = TestServer::start_castore();
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/nix-cache-info", server.base_url()))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("StoreDir: /nix/store"));
}

#[tokio::test]
async fn test_castore_push_pull_nar_e2e() {
    let server = TestServer::start_castore_with_tokens(&["writer"]);
    let client = reqwest::Client::new();
    let base = server.base_url();

    let nar_data = build_test_nar(b"hello from castore test");

    // Push NAR.
    let resp = client
        .put(format!("{base}/nar/cas111.nar"))
        .header("Authorization", "Bearer test_token_writer")
        .body(nar_data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Push narinfo.
    let narinfo = "StorePath: /nix/store/cas111-test-1.0\nURL: nar/cas111.nar\nCompression: \
                   none\nNarHash: sha256:aabbccdd\nNarSize: 200\nReferences: cas111-test-1.0\n";
    let resp = client
        .put(format!("{base}/cas111.narinfo"))
        .header("Authorization", "Bearer test_token_writer")
        .body(narinfo)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Read narinfo back.
    let resp = client
        .get(format!("{base}/cas111.narinfo"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("StorePath: /nix/store/cas111-test-1.0"));
    assert!(body.contains("NarHash: sha256:aabbccdd"));
    // Should be re-signed by the server.
    assert!(body.contains("Sig: test-cache-1:"));

    // Read NAR back (reconstructed from CAS chunks).
    let resp = client
        .get(format!("{base}/nar/cas111.nar"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let retrieved_nar = resp.bytes().await.unwrap();
    assert_eq!(retrieved_nar.as_ref(), nar_data.as_slice());
}

#[tokio::test]
async fn test_castore_narinfo_missing() {
    let server = TestServer::start_castore();
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/nonexistent.narinfo", server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_castore_nar_missing() {
    let server = TestServer::start_castore();
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/nar/nonexistent.nar", server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_castore_chunk_endpoint() {
    let server = TestServer::start_castore_with_tokens(&["writer"]);
    let client = reqwest::Client::new();
    let base = server.base_url();

    // Push a NAR to populate chunks.
    let nar_data = build_test_nar(b"chunk endpoint test data");
    let resp = client
        .put(format!("{base}/nar/chk111.nar"))
        .header("Authorization", "Bearer test_token_writer")
        .body(nar_data)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Push narinfo.
    let narinfo = "StorePath: /nix/store/chk111-test-1.0\nURL: nar/chk111.nar\nCompression: \
                   none\nNarHash: sha256:112233\nNarSize: 100\n";
    let resp = client
        .put(format!("{base}/chk111.narinfo"))
        .header("Authorization", "Bearer test_token_writer")
        .body(narinfo)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Compute the blake3 hash of the file content to find its chunk.
    let content = b"chunk endpoint test data";
    let hash = blake3::hash(content);
    let hex: String = hash.as_bytes().iter().map(|b| format!("{b:02x}")).collect();

    // GET the chunk.
    let resp = client
        .get(format!("{base}/cas/chunk/{hex}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let chunk_data = resp.bytes().await.unwrap();
    assert_eq!(chunk_data.as_ref(), content);
}

#[tokio::test]
async fn test_castore_chunk_not_found() {
    let server = TestServer::start_castore();
    let client = reqwest::Client::new();

    // Request a non-existent chunk.
    let fake_hex = "00".repeat(32);
    let resp = client
        .get(format!("{}/cas/chunk/{fake_hex}", server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_castore_push_requires_auth() {
    let server = TestServer::start_castore_with_tokens(&["ci"]);
    let client = reqwest::Client::new();
    let base = server.base_url();

    let nar_data = build_test_nar(b"auth test");

    // Without token — should fail.
    let resp = client
        .put(format!("{base}/nar/auth111.nar"))
        .body(nar_data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // With correct token — should succeed.
    let resp = client
        .put(format!("{base}/nar/auth111.nar"))
        .header("Authorization", "Bearer test_token_ci")
        .body(nar_data)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

// ===== gRPC StreamNars tests =====

#[tokio::test]
async fn test_stream_nars_basic() {
    let server = TestServer::start();
    let base = server.base_url();

    // Populate two NARs in the cache: dep first, then pkg that references it.
    let nar_a = b"nar-data-for-dep-aaa";
    let nar_b = b"nar-data-for-pkg-bbb";

    server.write_narinfo(
        "dep111",
        "StorePath: /nix/store/dep111-dep-1.0\nURL: nar/dep111.nar\nCompression: none\nNarHash: \
         sha256:aa11\nNarSize: 20\n",
    );
    server.write_nar("dep111.nar", nar_a);

    server.write_narinfo(
        "pkg222",
        "StorePath: /nix/store/pkg222-pkg-1.0\nURL: nar/pkg222.nar\nCompression: none\nNarHash: \
         sha256:bb22\nNarSize: 20\nReferences: dep111-dep-1.0\n",
    );
    server.write_nar("pkg222.nar", nar_b);

    // Connect gRPC and stream both NARs.
    use ekapkgs_protocol::ekapkgs::v1::StreamNarsRequest;
    use ekapkgs_protocol::ekapkgs::v1::cache_service_client::CacheServiceClient;

    let mut client = CacheServiceClient::connect(base.clone()).await.unwrap();
    let request = tonic::Request::new(StreamNarsRequest {
        path_hashes: vec!["dep111".to_owned(), "pkg222".to_owned()],
    });
    let mut stream = client.stream_nars(request).await.unwrap().into_inner();

    // Collect all chunks.
    let mut received: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();
    let mut last_seen: std::collections::HashMap<String, bool> = std::collections::HashMap::new();

    while let Some(chunk) = stream.message().await.unwrap() {
        received
            .entry(chunk.path_hash.clone())
            .or_default()
            .extend_from_slice(&chunk.data);
        if chunk.last {
            last_seen.insert(chunk.path_hash.clone(), true);
        }
    }

    // Verify we got both paths with correct data.
    assert_eq!(received.get("dep111").unwrap().as_slice(), nar_a);
    assert_eq!(received.get("pkg222").unwrap().as_slice(), nar_b);
    assert!(last_seen.get("dep111").copied().unwrap_or(false));
    assert!(last_seen.get("pkg222").copied().unwrap_or(false));
}

#[tokio::test]
async fn test_stream_nars_missing_path() {
    let server = TestServer::start();
    let base = server.base_url();

    // Populate one NAR.
    server.write_narinfo(
        "exists1",
        "StorePath: /nix/store/exists1-pkg-1.0\nURL: nar/exists1.nar\nCompression: none\nNarHash: \
         sha256:ee11\nNarSize: 10\n",
    );
    server.write_nar("exists1.nar", b"nar-exists");

    use ekapkgs_protocol::ekapkgs::v1::StreamNarsRequest;
    use ekapkgs_protocol::ekapkgs::v1::cache_service_client::CacheServiceClient;

    let mut client = CacheServiceClient::connect(base.clone()).await.unwrap();
    let request = tonic::Request::new(StreamNarsRequest {
        path_hashes: vec!["nonexistent".to_owned(), "exists1".to_owned()],
    });
    let mut stream = client.stream_nars(request).await.unwrap().into_inner();

    // Should skip the missing path and deliver the existing one.
    let mut received: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();

    while let Some(chunk) = stream.message().await.unwrap() {
        received
            .entry(chunk.path_hash.clone())
            .or_default()
            .extend_from_slice(&chunk.data);
    }

    assert!(!received.contains_key("nonexistent"));
    assert_eq!(received.get("exists1").unwrap().as_slice(), b"nar-exists");
}

#[tokio::test]
async fn test_stream_nars_castore() {
    let server = TestServer::start_castore_with_tokens(&["writer"]);
    let base = server.base_url();
    let client = reqwest::Client::new();

    // Push a NAR via HTTP (castore backend decomposes it into chunks).
    let nar_data = build_test_nar(b"streamed from castore");
    let resp = client
        .put(format!("{base}/nar/cas222.nar"))
        .header("Authorization", "Bearer test_token_writer")
        .body(nar_data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let narinfo = "StorePath: /nix/store/cas222-test-1.0\nURL: nar/cas222.nar\nCompression: \
                   none\nNarHash: sha256:ccdd\nNarSize: 100\n";
    let resp = client
        .put(format!("{base}/cas222.narinfo"))
        .header("Authorization", "Bearer test_token_writer")
        .body(narinfo)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Now stream it back via gRPC.
    use ekapkgs_protocol::ekapkgs::v1::StreamNarsRequest;
    use ekapkgs_protocol::ekapkgs::v1::cache_service_client::CacheServiceClient;

    let mut grpc_client = CacheServiceClient::connect(base.clone()).await.unwrap();
    let request = tonic::Request::new(StreamNarsRequest {
        path_hashes: vec!["cas222".to_owned()],
    });
    let mut stream = grpc_client.stream_nars(request).await.unwrap().into_inner();

    let mut received = Vec::new();
    while let Some(chunk) = stream.message().await.unwrap() {
        assert_eq!(chunk.path_hash, "cas222");
        received.extend_from_slice(&chunk.data);
    }

    // The NAR reconstructed from CAS should match the original.
    assert_eq!(received, nar_data);
}

#[tokio::test]
async fn test_stream_nars_file_size_on_first_chunk() {
    let server = TestServer::start();
    let base = server.base_url();

    let nar_data = vec![0xABu8; 200_000]; // > 64 KiB so multiple chunks
    server.write_narinfo(
        "big111",
        "StorePath: /nix/store/big111-big-1.0\nURL: nar/big111.nar\nCompression: none\nNarHash: \
         sha256:bbig\nNarSize: 200000\n",
    );
    server.write_nar("big111.nar", &nar_data);

    use ekapkgs_protocol::ekapkgs::v1::StreamNarsRequest;
    use ekapkgs_protocol::ekapkgs::v1::cache_service_client::CacheServiceClient;

    let mut client = CacheServiceClient::connect(base.clone()).await.unwrap();
    let request = tonic::Request::new(StreamNarsRequest {
        path_hashes: vec!["big111".to_owned()],
    });
    let mut stream = client.stream_nars(request).await.unwrap().into_inner();

    let mut chunks_received = 0u32;
    let mut total_bytes = 0usize;
    let mut first_file_size = 0u64;

    while let Some(chunk) = stream.message().await.unwrap() {
        if chunks_received == 0 {
            first_file_size = chunk.file_size;
        } else {
            // file_size should be 0 on subsequent chunks.
            assert_eq!(chunk.file_size, 0);
        }
        total_bytes += chunk.data.len();
        chunks_received += 1;
    }

    assert!(chunks_received > 1, "should have multiple chunks");
    assert_eq!(first_file_size, 200_000);
    assert_eq!(total_bytes, 200_000);
}
