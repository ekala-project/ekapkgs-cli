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
    // Hash must be exactly 32 chars of nix base32 (no e,o,t,u).
    let hash = "0123456789abcdfghijklmnpqrsvwxyz";
    server.write_narinfo(
        hash,
        &format!(
            "StorePath: /nix/store/{hash}-hello-1.0\nURL: nar/{hash}.nar\nCompression: \
             none\nNarHash: sha256:deadbeef\nNarSize: 100\n"
        ),
    );

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/{hash}.narinfo", server.base_url()))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains(&format!("StorePath: /nix/store/{hash}-hello-1.0")));
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
    let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaabb";

    let narinfo = format!(
        "StorePath: /nix/store/{hash}-pkg-1.0\nURL: nar/{hash}.nar\nCompression: none\nNarHash: \
         sha256:aabbccdd\nNarSize: 200\n"
    );

    // Without token — should fail.
    let resp = client
        .put(format!("{}/{hash}.narinfo", server.base_url()))
        .body(narinfo.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // With wrong token — should fail.
    let resp = client
        .put(format!("{}/{hash}.narinfo", server.base_url()))
        .header("Authorization", "Bearer wrong_token")
        .body(narinfo.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // With correct token — should succeed.
    let resp = client
        .put(format!("{}/{hash}.narinfo", server.base_url()))
        .header("Authorization", "Bearer test_token_ci")
        .body(narinfo)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Verify it's now readable.
    let resp = client
        .get(format!("{}/{hash}.narinfo", server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains(&format!("StorePath: /nix/store/{hash}-pkg-1.0")));
}

#[tokio::test]
async fn test_push_nar_and_narinfo_e2e() {
    let server = TestServer::start_with_tokens(&["writer"]);
    let client = reqwest::Client::new();
    let base = server.base_url();
    let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa11";

    let nar_data = b"test-nar-binary-content";
    let narinfo = format!(
        "StorePath: /nix/store/{hash}-test-1.0\nURL: nar/{hash}.nar\nCompression: none\nNarHash: \
         sha256:112233\nNarSize: 50\nReferences: {hash}-test-1.0\n"
    );

    // Push NAR.
    let resp = client
        .put(format!("{base}/nar/{hash}.nar"))
        .header("Authorization", "Bearer test_token_writer")
        .body(nar_data.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Push narinfo.
    let resp = client
        .put(format!("{base}/{hash}.narinfo"))
        .header("Authorization", "Bearer test_token_writer")
        .body(narinfo)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Verify both are readable.
    let resp = client
        .get(format!("{base}/{hash}.narinfo"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("NarHash: sha256:112233"));
    assert!(body.contains("Sig: test-cache-1:"));

    let resp = client
        .get(format!("{base}/nar/{hash}.nar"))
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
    let hash = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbb11";
    let missing = "cccccccccccccccccccccccccccccc22";
    server.write_narinfo(
        hash,
        &format!(
            "StorePath: /nix/store/{hash}-pkg-1.0\nURL: nar/{hash}.nar\nNarHash: \
             sha256:aabb\nNarSize: 10\n"
        ),
    );

    let client = reqwest::Client::new();

    // HEAD existing — 200.
    let resp = client
        .head(format!("{}/{hash}.narinfo", server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // HEAD missing — 404.
    let resp = client
        .head(format!("{}/{missing}.narinfo", server.base_url()))
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
    let narinfo_bob =
        "StorePath: /nix/store/multi2-1.0\nURL: nar/multi2.nar\nNarHash: sha256:ff\nNarSize: 1\n";
    let resp = client
        .put(format!("{}/multi2.narinfo", server.base_url()))
        .header("Authorization", "Bearer test_token_bob")
        .body(narinfo_bob)
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
    let hash = "dddddddddddddddddddddddddddddddd";

    let nar_data = build_test_nar(b"hello from castore test");

    // Push NAR.
    let resp = client
        .put(format!("{base}/nar/{hash}.nar"))
        .header("Authorization", "Bearer test_token_writer")
        .body(nar_data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Push narinfo.
    let narinfo = format!(
        "StorePath: /nix/store/{hash}-test-1.0\nURL: nar/{hash}.nar\nCompression: none\nNarHash: \
         sha256:aabbccdd\nNarSize: 200\nReferences: {hash}-test-1.0\n"
    );
    let resp = client
        .put(format!("{base}/{hash}.narinfo"))
        .header("Authorization", "Bearer test_token_writer")
        .body(narinfo)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Read narinfo back.
    let resp = client
        .get(format!("{base}/{hash}.narinfo"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains(&format!("StorePath: /nix/store/{hash}-test-1.0")));
    assert!(body.contains("NarHash: sha256:aabbccdd"));
    // Should be re-signed by the server.
    assert!(body.contains("Sig: test-cache-1:"));

    // Read NAR back (reconstructed from CAS chunks).
    let resp = client
        .get(format!("{base}/nar/{hash}.nar"))
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

// ===== Metrics tests =====

#[tokio::test]
async fn test_metrics_endpoint() {
    let server = TestServer::start();
    let client = reqwest::Client::new();
    let hash = "fffffffffffffffffffffffffffffff0";
    let miss = "fffffffffffffffffffffffffffffff1";

    // Write a narinfo and fetch it to generate some metrics.
    server.write_narinfo(
        hash,
        &format!(
            "StorePath: /nix/store/{hash}-pkg-1.0\nURL: nar/{hash}.nar\nCompression: \
             none\nNarHash: sha256:met1\nNarSize: 10\n"
        ),
    );
    server.write_nar(&format!("{hash}.nar"), b"nar-data");

    // Fetch narinfo to increment counters.
    let resp = client
        .get(format!("{}/{hash}.narinfo", server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Fetch NAR.
    let resp = client
        .get(format!("{}/nar/{hash}.nar", server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Fetch a missing narinfo to increment miss counter.
    let _ = client
        .get(format!("{}/{miss}.narinfo", server.base_url()))
        .send()
        .await
        .unwrap();

    // Check metrics endpoint.
    let resp = client
        .get(format!("{}/metrics", server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();

    // Verify expected metrics are present.
    assert!(body.contains("ekapkgs_negotiate_requests_total"));
    assert!(body.contains("ekapkgs_narinfo_requests_total"));
    assert!(body.contains("ekapkgs_nar_downloads_total"));
    assert!(body.contains("ekapkgs_push_narinfo_total"));
    assert!(body.contains("ekapkgs_gc_runs_total"));
    assert!(body.contains("ekapkgs_cache_size_bytes"));

    // Verify counters were incremented.
    assert!(body.contains(r#"ekapkgs_narinfo_requests_total{status="hit"} 1"#));
    assert!(body.contains(r#"ekapkgs_narinfo_requests_total{status="miss"} 1"#));
    assert!(body.contains("ekapkgs_nar_downloads_total 1"));
}

// ===== Delta transfer tests =====

#[tokio::test]
async fn test_delta_negotiate() {
    let server = TestServer::start_with_tokens(&["writer"]);
    let client = reqwest::Client::new();
    let base = server.base_url();

    // Push two versions of the same "package" — they share a pname.
    // Old version:
    let old_nar_data = build_test_nar(b"shared content between versions, old version data here!");
    let resp = client
        .put(format!("{base}/nar/old111.nar"))
        .header("Authorization", "Bearer test_token_writer")
        .body(old_nar_data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let old_narinfo = format!(
        "StorePath: /nix/store/old111-mypkg-1.0\nURL: nar/old111.nar\nCompression: none\nNarHash: \
         sha256:old1\nNarSize: {}\nFileSize: {}\n",
        old_nar_data.len(),
        old_nar_data.len()
    );
    let resp = client
        .put(format!("{base}/old111.narinfo"))
        .header("Authorization", "Bearer test_token_writer")
        .body(old_narinfo)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // New version (same pname "mypkg", different hash/version):
    let new_nar_data = build_test_nar(b"shared content between versions, new version data here!");
    let resp = client
        .put(format!("{base}/nar/new222.nar"))
        .header("Authorization", "Bearer test_token_writer")
        .body(new_nar_data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let new_narinfo = format!(
        "StorePath: /nix/store/new222-mypkg-2.0\nURL: nar/new222.nar\nCompression: none\nNarHash: \
         sha256:new2\nNarSize: {}\nFileSize: {}\n",
        new_nar_data.len(),
        new_nar_data.len()
    );
    let resp = client
        .put(format!("{base}/new222.narinfo"))
        .header("Authorization", "Bearer test_token_writer")
        .body(new_narinfo)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Negotiate: client has old version, wants new version.
    use ekapkgs_protocol::ekapkgs::v1::cache_service_client::CacheServiceClient;
    use ekapkgs_protocol::ekapkgs::v1::{Compression, NegotiateRequest};

    let mut grpc_client = CacheServiceClient::connect(base.clone()).await.unwrap();
    let request = tonic::Request::new(NegotiateRequest {
        want: vec!["new222".to_owned()],
        have: vec!["old111".to_owned()],
        accept_compression: vec![Compression::Zstd as i32],
        trust_roots: Vec::new(),
        supports_cas: false,
        target_hash: String::new(),
    });
    let response = grpc_client.negotiate(request).await.unwrap().into_inner();

    assert_eq!(response.available.len(), 1);
    let entry = &response.available[0];

    // Should have delta fields populated.
    assert_eq!(entry.delta_base_hash, "old111");
    assert!(!entry.delta_url.is_empty());
    assert!(entry.delta_size > 0);
    // Delta should be smaller than the full NAR.
    assert!(entry.delta_size < entry.file_size);
}

#[tokio::test]
async fn test_delta_http_download() {
    let server = TestServer::start_with_tokens(&["writer"]);
    let client = reqwest::Client::new();
    let base = server.base_url();

    // Push two versions.
    let old_nar = build_test_nar(b"shared package content - the original version of the pkg");
    let resp = client
        .put(format!("{base}/nar/dold1.nar"))
        .header("Authorization", "Bearer test_token_writer")
        .body(old_nar.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resp = client
        .put(format!("{base}/dold1.narinfo"))
        .header("Authorization", "Bearer test_token_writer")
        .body(format!(
            "StorePath: /nix/store/dold1-deltapkg-1.0\nURL: nar/dold1.nar\nCompression: \
             none\nNarHash: sha256:do1\nNarSize: {}\nFileSize: {}\n",
            old_nar.len(),
            old_nar.len()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let new_nar = build_test_nar(b"shared package content - the updated version of the pkg");
    let resp = client
        .put(format!("{base}/nar/dnew2.nar"))
        .header("Authorization", "Bearer test_token_writer")
        .body(new_nar.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resp = client
        .put(format!("{base}/dnew2.narinfo"))
        .header("Authorization", "Bearer test_token_writer")
        .body(format!(
            "StorePath: /nix/store/dnew2-deltapkg-2.0\nURL: nar/dnew2.nar\nCompression: \
             none\nNarHash: sha256:dn2\nNarSize: {}\nFileSize: {}\n",
            new_nar.len(),
            new_nar.len()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Negotiate to populate delta cache.
    use ekapkgs_protocol::ekapkgs::v1::cache_service_client::CacheServiceClient;
    use ekapkgs_protocol::ekapkgs::v1::{Compression, NegotiateRequest};

    let mut grpc_client = CacheServiceClient::connect(base.clone()).await.unwrap();
    let request = tonic::Request::new(NegotiateRequest {
        want: vec!["dnew2".to_owned()],
        have: vec!["dold1".to_owned()],
        accept_compression: vec![Compression::Zstd as i32],
        trust_roots: Vec::new(),
        supports_cas: false,
        target_hash: String::new(),
    });
    let response = grpc_client.negotiate(request).await.unwrap().into_inner();
    let entry = &response.available[0];
    assert!(!entry.delta_url.is_empty());

    // Download the delta via HTTP.
    let delta_resp = client
        .get(format!("{base}/{}", entry.delta_url))
        .send()
        .await
        .unwrap();
    assert_eq!(delta_resp.status(), 200);
    let delta_bytes = delta_resp.bytes().await.unwrap();

    // Decompress using old NAR as dictionary to reconstruct new NAR.
    use std::io::Read;
    let mut decoder = zstd::Decoder::with_dictionary(&delta_bytes[..], &old_nar).unwrap();
    let mut reconstructed = Vec::new();
    decoder.read_to_end(&mut reconstructed).unwrap();

    assert_eq!(reconstructed, new_nar);
}

#[tokio::test]
async fn test_delta_stream() {
    let server = TestServer::start_with_tokens(&["writer"]);
    let client = reqwest::Client::new();
    let base = server.base_url();

    // Push two versions.
    let old_nar = build_test_nar(b"streaming delta test - base version of the package content!");
    let resp = client
        .put(format!("{base}/nar/sold1.nar"))
        .header("Authorization", "Bearer test_token_writer")
        .body(old_nar.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resp = client
        .put(format!("{base}/sold1.narinfo"))
        .header("Authorization", "Bearer test_token_writer")
        .body(format!(
            "StorePath: /nix/store/sold1-streampkg-1.0\nURL: nar/sold1.nar\nCompression: \
             none\nNarHash: sha256:so1\nNarSize: {}\nFileSize: {}\n",
            old_nar.len(),
            old_nar.len()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let new_nar = build_test_nar(b"streaming delta test - new! version of the package content!");
    let resp = client
        .put(format!("{base}/nar/snew2.nar"))
        .header("Authorization", "Bearer test_token_writer")
        .body(new_nar.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resp = client
        .put(format!("{base}/snew2.narinfo"))
        .header("Authorization", "Bearer test_token_writer")
        .body(format!(
            "StorePath: /nix/store/snew2-streampkg-2.0\nURL: nar/snew2.nar\nCompression: \
             none\nNarHash: sha256:sn2\nNarSize: {}\nFileSize: {}\n",
            new_nar.len(),
            new_nar.len()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Negotiate to populate delta cache.
    use ekapkgs_protocol::ekapkgs::v1::cache_service_client::CacheServiceClient;
    use ekapkgs_protocol::ekapkgs::v1::{Compression, NegotiateRequest, StreamNarsRequest};

    let mut grpc_client = CacheServiceClient::connect(base.clone()).await.unwrap();
    let request = tonic::Request::new(NegotiateRequest {
        want: vec!["snew2".to_owned()],
        have: vec!["sold1".to_owned()],
        accept_compression: vec![Compression::Zstd as i32],
        trust_roots: Vec::new(),
        supports_cas: false,
        target_hash: String::new(),
    });
    let response = grpc_client.negotiate(request).await.unwrap().into_inner();
    assert!(!response.available[0].delta_base_hash.is_empty());

    // Stream the NAR — should receive delta bytes.
    let request = tonic::Request::new(StreamNarsRequest {
        path_hashes: vec!["snew2".to_owned()],
    });
    let mut stream = grpc_client.stream_nars(request).await.unwrap().into_inner();

    let mut received = Vec::new();
    let mut is_delta = false;
    while let Some(chunk) = stream.message().await.unwrap() {
        is_delta = chunk.is_delta;
        received.extend_from_slice(&chunk.data);
    }

    assert!(is_delta, "stream should have sent delta bytes");

    // Decompress using old NAR as dictionary.
    use std::io::Read;
    let mut decoder = zstd::Decoder::with_dictionary(&received[..], &old_nar).unwrap();
    let mut reconstructed = Vec::new();
    decoder.read_to_end(&mut reconstructed).unwrap();

    assert_eq!(reconstructed, new_nar);
}

// ===== CAS advanced integration tests =====

/// Build a NAR containing a directory with multiple files.
fn build_dir_test_nar(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    nar_write_str(&mut buf, "nix-archive-1");
    nar_write_str(&mut buf, "(");
    nar_write_str(&mut buf, "type");
    nar_write_str(&mut buf, "directory");

    // Files must be sorted by name for valid NAR.
    let mut sorted_files: Vec<_> = files.to_vec();
    sorted_files.sort_by_key(|(name, _)| *name);

    for (name, content) in &sorted_files {
        nar_write_str(&mut buf, "entry");
        nar_write_str(&mut buf, "(");
        nar_write_str(&mut buf, "name");
        nar_write_str(&mut buf, name);
        nar_write_str(&mut buf, "node");
        nar_write_str(&mut buf, "(");
        nar_write_str(&mut buf, "type");
        nar_write_str(&mut buf, "regular");
        nar_write_str(&mut buf, "contents");
        nar_write_bytes(&mut buf, content);
        nar_write_str(&mut buf, ")");
        nar_write_str(&mut buf, ")");
    }

    nar_write_str(&mut buf, ")");
    buf
}

/// Helper: push a NAR + narinfo to a castore server.
async fn push_nar_to_castore(
    client: &reqwest::Client,
    base: &str,
    hash: &str,
    pname: &str,
    version: &str,
    nar_data: &[u8],
    token: &str,
) {
    let resp = client
        .put(format!("{base}/nar/{hash}.nar"))
        .header("Authorization", format!("Bearer {token}"))
        .body(nar_data.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "NAR upload failed for {hash}");

    let nar_hash = {
        use sha2::Digest;
        let h = sha2::Sha256::digest(nar_data);
        h.iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    let narinfo = format!(
        "StorePath: /nix/store/{hash}-{pname}-{version}\nURL: nar/{hash}.nar\nCompression: \
         none\nNarHash: sha256:{nar_hash}\nNarSize: {}\nFileSize: {}\nReferences: \
         {hash}-{pname}-{version}\n",
        nar_data.len(),
        nar_data.len()
    );
    let resp = client
        .put(format!("{base}/{hash}.narinfo"))
        .header("Authorization", format!("Bearer {token}"))
        .body(narinfo)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "narinfo upload failed for {hash}");
}

#[tokio::test]
async fn test_castore_directory_nar_roundtrip() {
    let server = TestServer::start_castore_with_tokens(&["writer"]);
    let client = reqwest::Client::new();
    let base = server.base_url();

    let nar_data = build_dir_test_nar(&[
        ("hello.txt", b"Hello, world!"),
        ("script.sh", b"#!/bin/sh\necho hi\n"),
    ]);

    push_nar_to_castore(
        &client,
        &base,
        "dir111",
        "dirpkg",
        "1.0",
        &nar_data,
        "test_token_writer",
    )
    .await;

    // Read NAR back — should be reconstructed from CAS tree.
    let resp = client
        .get(format!("{base}/nar/dir111.nar"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let retrieved = resp.bytes().await.unwrap();
    assert_eq!(retrieved.as_ref(), nar_data.as_slice());
}

#[tokio::test]
async fn test_castore_chunk_dedup_across_paths() {
    let server = TestServer::start_castore_with_tokens(&["writer"]);
    let client = reqwest::Client::new();
    let base = server.base_url();

    // Two NARs with identical file content.
    let shared_content = b"identical content shared across two packages";
    let nar1 = build_test_nar(shared_content);
    let nar2 = build_test_nar(shared_content);

    push_nar_to_castore(
        &client,
        &base,
        "dup111",
        "pkga",
        "1.0",
        &nar1,
        "test_token_writer",
    )
    .await;
    push_nar_to_castore(
        &client,
        &base,
        "dup222",
        "pkgb",
        "1.0",
        &nar2,
        "test_token_writer",
    )
    .await;

    // Both should reconstruct correctly.
    let resp1 = client
        .get(format!("{base}/nar/dup111.nar"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp1.bytes().await.unwrap().as_ref(), nar1.as_slice());

    let resp2 = client
        .get(format!("{base}/nar/dup222.nar"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.bytes().await.unwrap().as_ref(), nar2.as_slice());

    // The chunk for the shared content should exist and be fetchable.
    let hash = blake3::hash(shared_content);
    let hex: String = hash.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let resp = client
        .get(format!("{base}/cas/chunk/{hex}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.bytes().await.unwrap().as_ref(), shared_content);
}

#[tokio::test]
async fn test_castore_negotiate_with_cas_support() {
    let server = TestServer::start_castore_with_tokens(&["writer"]);
    let client = reqwest::Client::new();
    let base = server.base_url();

    let nar_data = build_test_nar(b"negotiate cas test content");
    push_nar_to_castore(
        &client,
        &base,
        "neg111",
        "negpkg",
        "1.0",
        &nar_data,
        "test_token_writer",
    )
    .await;

    // Negotiate with supports_cas=true — should return ca_path_mappings.
    use ekapkgs_protocol::ekapkgs::v1::NegotiateRequest;
    use ekapkgs_protocol::ekapkgs::v1::cache_service_client::CacheServiceClient;

    let mut grpc_client = CacheServiceClient::connect(base.clone())
        .await
        .unwrap()
        .max_decoding_message_size(64 * 1024 * 1024);
    let request = tonic::Request::new(NegotiateRequest {
        want: vec!["neg111".to_owned()],
        have: Vec::new(),
        accept_compression: Vec::new(),
        trust_roots: Vec::new(),
        supports_cas: true,
        target_hash: String::new(),
    });
    let response = grpc_client.negotiate(request).await.unwrap().into_inner();

    assert_eq!(response.available.len(), 1);
    assert_eq!(response.ca_path_mappings.len(), 1);
    assert_eq!(response.ca_path_mappings[0].store_path_hash, "neg111");
    assert!(response.ca_path_mappings[0].root_node.is_some());
}

#[tokio::test]
async fn test_castore_negotiate_chunks_rpc() {
    let server = TestServer::start_castore_with_tokens(&["writer"]);
    let client = reqwest::Client::new();
    let base = server.base_url();

    let nar_data = build_test_nar(b"chunk negotiation test data");
    push_nar_to_castore(
        &client,
        &base,
        "cneg11",
        "chunkpkg",
        "1.0",
        &nar_data,
        "test_token_writer",
    )
    .await;

    use ekapkgs_protocol::ekapkgs::v1::ChunkNegotiateRequest;
    use ekapkgs_protocol::ekapkgs::v1::cache_service_client::CacheServiceClient;

    let mut grpc_client = CacheServiceClient::connect(base.clone())
        .await
        .unwrap()
        .max_decoding_message_size(64 * 1024 * 1024);
    let request = tonic::Request::new(ChunkNegotiateRequest {
        want: vec!["cneg11".to_owned()],
        have_chunks: Vec::new(),
        have: Vec::new(),
    });
    let response = grpc_client
        .negotiate_chunks(request)
        .await
        .unwrap()
        .into_inner();

    assert_eq!(response.path_mappings.len(), 1);
    assert_eq!(response.path_mappings[0].store_path_hash, "cneg11");
    assert!(response.path_mappings[0].root_node.is_some());
    assert!(!response.missing_chunks.is_empty());
    assert!(response.total_chunk_size > 0);
    assert!(!response.file_chunk_mappings.is_empty());

    for chunk in &response.missing_chunks {
        assert!(chunk.digest.is_some());
        assert!(chunk.size > 0);
        assert!(chunk.url.starts_with("cas/chunk/"));
    }
}

#[tokio::test]
async fn test_castore_negotiate_chunks_with_existing() {
    let server = TestServer::start_castore_with_tokens(&["writer"]);
    let client = reqwest::Client::new();
    let base = server.base_url();

    let content = b"content that client already has cached";
    let nar_data = build_test_nar(content);
    push_nar_to_castore(
        &client,
        &base,
        "cneg22",
        "cached",
        "1.0",
        &nar_data,
        "test_token_writer",
    )
    .await;

    let chunk_hash = blake3::hash(content);
    let have_digest = ekapkgs_protocol::ekapkgs::v1::B3Digest {
        digest: chunk_hash.as_bytes().to_vec(),
    };

    use ekapkgs_protocol::ekapkgs::v1::ChunkNegotiateRequest;
    use ekapkgs_protocol::ekapkgs::v1::cache_service_client::CacheServiceClient;

    let mut grpc_client = CacheServiceClient::connect(base.clone())
        .await
        .unwrap()
        .max_decoding_message_size(64 * 1024 * 1024);
    let request = tonic::Request::new(ChunkNegotiateRequest {
        want: vec!["cneg22".to_owned()],
        have_chunks: vec![have_digest],
        have: Vec::new(),
    });
    let response = grpc_client
        .negotiate_chunks(request)
        .await
        .unwrap()
        .into_inner();

    assert_eq!(response.path_mappings.len(), 1);
    assert!(
        response.missing_chunks.is_empty(),
        "client has all chunks, missing should be empty but got {}",
        response.missing_chunks.len()
    );
    assert_eq!(response.total_chunk_size, 0);
}

#[tokio::test]
async fn test_castore_chunk_upload_with_verification() {
    let server = TestServer::start_castore_with_tokens(&["writer"]);
    let client = reqwest::Client::new();
    let base = server.base_url();

    let chunk_data = b"externally uploaded chunk data";
    let hash = blake3::hash(chunk_data);
    let hex: String = hash.as_bytes().iter().map(|b| format!("{b:02x}")).collect();

    let resp = client
        .put(format!("{base}/cas/chunk/{hex}"))
        .header("Authorization", "Bearer test_token_writer")
        .body(chunk_data.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = client
        .get(format!("{base}/cas/chunk/{hex}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.bytes().await.unwrap().as_ref(), chunk_data);

    // Idempotent re-upload.
    let resp = client
        .put(format!("{base}/cas/chunk/{hex}"))
        .header("Authorization", "Bearer test_token_writer")
        .body(chunk_data.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_castore_chunk_upload_digest_mismatch() {
    let server = TestServer::start_castore_with_tokens(&["writer"]);
    let client = reqwest::Client::new();
    let base = server.base_url();

    let chunk_data = b"this data does not match the digest";
    let wrong_hex = "aa".repeat(32);

    let resp = client
        .put(format!("{base}/cas/chunk/{wrong_hex}"))
        .header("Authorization", "Bearer test_token_writer")
        .body(chunk_data.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body = resp.text().await.unwrap();
    assert!(body.contains("digest mismatch"));
}

#[tokio::test]
async fn test_castore_negotiate_chunks_unavailable() {
    let server = TestServer::start_castore();

    use ekapkgs_protocol::ekapkgs::v1::ChunkNegotiateRequest;
    use ekapkgs_protocol::ekapkgs::v1::cache_service_client::CacheServiceClient;

    let mut grpc_client = CacheServiceClient::connect(server.base_url())
        .await
        .unwrap()
        .max_decoding_message_size(64 * 1024 * 1024);
    let request = tonic::Request::new(ChunkNegotiateRequest {
        want: vec!["nonexistent".to_owned()],
        have_chunks: Vec::new(),
        have: Vec::new(),
    });
    let response = grpc_client
        .negotiate_chunks(request)
        .await
        .unwrap()
        .into_inner();

    assert!(response.path_mappings.is_empty());
    assert_eq!(response.unavailable, vec!["nonexistent"]);
    assert!(response.missing_chunks.is_empty());
}

#[tokio::test]
async fn test_castore_metrics_include_cas() {
    let server = TestServer::start_castore_with_tokens(&["writer"]);
    let client = reqwest::Client::new();
    let base = server.base_url();

    let nar_data = build_test_nar(b"metrics test");
    push_nar_to_castore(
        &client,
        &base,
        "met222",
        "metpkg",
        "1.0",
        &nar_data,
        "test_token_writer",
    )
    .await;

    let resp = client.get(format!("{base}/metrics")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();

    assert!(
        body.contains("ekapkgs_cas_chunks_total"),
        "missing cas_chunks_total"
    );
    assert!(
        body.contains("ekapkgs_cas_chunks_bytes_total"),
        "missing cas_chunks_bytes_total"
    );
    assert!(
        body.contains("ekapkgs_cas_paths_total"),
        "missing cas_paths_total"
    );
    assert!(
        body.contains("ekapkgs_cas_push_chunks_new"),
        "missing cas_push_chunks_new"
    );
    assert!(
        body.contains("ekapkgs_cas_push_chunks_existing"),
        "missing cas_push_chunks_existing"
    );
}

#[tokio::test]
async fn test_castore_stream_nars_directory() {
    let server = TestServer::start_castore_with_tokens(&["writer"]);
    let client = reqwest::Client::new();
    let base = server.base_url();

    let nar_data = build_dir_test_nar(&[
        ("bin/hello", b"#!/bin/sh\necho hello\n"),
        ("lib/libfoo.so", b"fake shared library data"),
    ]);
    push_nar_to_castore(
        &client,
        &base,
        "sdir11",
        "dirstream",
        "1.0",
        &nar_data,
        "test_token_writer",
    )
    .await;

    use ekapkgs_protocol::ekapkgs::v1::StreamNarsRequest;
    use ekapkgs_protocol::ekapkgs::v1::cache_service_client::CacheServiceClient;

    let mut grpc_client = CacheServiceClient::connect(base.clone()).await.unwrap();
    let request = tonic::Request::new(StreamNarsRequest {
        path_hashes: vec!["sdir11".to_owned()],
    });
    let mut stream = grpc_client.stream_nars(request).await.unwrap().into_inner();

    let mut received = Vec::new();
    while let Some(chunk) = stream.message().await.unwrap() {
        received.extend_from_slice(&chunk.data);
    }

    assert_eq!(received, nar_data);
}

#[tokio::test]
async fn test_castore_negotiate_without_cas_support() {
    let server = TestServer::start_castore_with_tokens(&["writer"]);
    let client = reqwest::Client::new();
    let base = server.base_url();

    let nar_data = build_test_nar(b"no cas flag test");
    push_nar_to_castore(
        &client,
        &base,
        "noca11",
        "nocaspkg",
        "1.0",
        &nar_data,
        "test_token_writer",
    )
    .await;

    use ekapkgs_protocol::ekapkgs::v1::NegotiateRequest;
    use ekapkgs_protocol::ekapkgs::v1::cache_service_client::CacheServiceClient;

    let mut grpc_client = CacheServiceClient::connect(base.clone())
        .await
        .unwrap()
        .max_decoding_message_size(64 * 1024 * 1024);
    let request = tonic::Request::new(NegotiateRequest {
        want: vec!["noca11".to_owned()],
        have: Vec::new(),
        accept_compression: Vec::new(),
        trust_roots: Vec::new(),
        supports_cas: false,
        target_hash: String::new(),
    });
    let response = grpc_client.negotiate(request).await.unwrap().into_inner();

    assert_eq!(response.available.len(), 1);
    assert!(
        response.ca_path_mappings.is_empty(),
        "ca_path_mappings should be empty when supports_cas=false"
    );
}
