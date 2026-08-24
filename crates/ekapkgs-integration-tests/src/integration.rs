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
        "StorePath: /nix/store/abc123-hello-1.0\n\
         URL: nar/abc123.nar\n\
         Compression: none\n\
         NarHash: sha256:deadbeef\n\
         NarSize: 100\n",
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

    let narinfo = "StorePath: /nix/store/xyz789-pkg-1.0\n\
                   URL: nar/xyz789.nar\n\
                   Compression: none\n\
                   NarHash: sha256:aabbccdd\n\
                   NarSize: 200\n";

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
    let narinfo = "StorePath: /nix/store/aaa111-test-1.0\n\
                   URL: nar/aaa111.nar\n\
                   Compression: none\n\
                   NarHash: sha256:112233\n\
                   NarSize: 50\n\
                   References: aaa111-test-1.0\n";

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
        "StorePath: /nix/store/head123-pkg-1.0\n\
         URL: nar/head123.nar\n\
         NarHash: sha256:aabb\n\
         NarSize: 10\n",
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

    let narinfo = "StorePath: /nix/store/multi-1.0\n\
                   URL: nar/multi.nar\n\
                   NarHash: sha256:ff\n\
                   NarSize: 1\n";

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
