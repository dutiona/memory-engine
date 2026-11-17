//! The shared HTTP harness: the real `http::serve` in-process, with a wiremock embedder,
//! and a runner for the built binary. Tests drive both with plain `reqwest`, never with
//! rmcp's client, which `deny.toml` bans.
#![allow(dead_code, reason = "each test binary uses part of the harness")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use memory_engine::MemoryEngine;
use memory_engine_mcp::auth::TokenTable;
use memory_engine_mcp::embedding::HttpEmbeddingProvider;
use memory_engine_mcp::http;
use memory_engine_mcp::server::MemoryMcpServer;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub const DIM: usize = 4;
pub const MACHINES: [&str; 2] = ["laptop", "desktop"];

/// A token-shaped value for `machine`, built at run time so the repository holds no token.
#[must_use]
pub fn token(machine: &str) -> String {
    format!("me_{machine:0>32}")
}

fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(s.as_bytes())
        .iter()
        .fold(String::new(), |mut out, b| {
            use std::fmt::Write;
            write!(out, "{b:02x}").unwrap();
            out
        })
}

/// One token-hash file line for `machine`.
#[must_use]
pub fn token_file_line(machine: &str) -> String {
    format!("{machine} {}\n", sha256_hex(&token(machine)))
}

/// Write a token-hash file admitting `machines` and return its path.
pub fn write_token_file(dir: &Path, machines: &[&str]) -> PathBuf {
    let file = dir.join("tokens");
    let body: String = machines.iter().map(|m| token_file_line(m)).collect();
    std::fs::write(&file, body).unwrap();
    file
}

/// The JSON-RPC body of one MCP request.
#[must_use]
pub fn jsonrpc(method: &str, params: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
}

/// `initialize` params a client sends.
#[must_use]
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": { "name": "harness", "version": "0" }
    })
}

/// The in-process server, on an ephemeral loopback port.
pub struct TestServer {
    pub url: String,
    pub client: reqwest::Client,
    pub engine: Arc<MemoryEngine>,
    pub embedder: MockServer,
    pub dir: TempDir,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<std::io::Result<()>>>,
}

impl TestServer {
    pub async fn start() -> Self {
        Self::start_with_hosts(&[]).await
    }

    pub async fn start_with_hosts(allowed_hosts: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let embedder = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "data": [{ "embedding": vec![0.25_f32; DIM] }] })),
            )
            .mount(&embedder)
            .await;
        let endpoint = format!("{}/v1/embeddings", embedder.uri());
        let db = dir.path().join("memory.db");
        // The blocking reqwest client inside the provider must not be built on the runtime.
        let (engine, provider) = tokio::task::spawn_blocking(move || {
            let provider =
                HttpEmbeddingProvider::new(endpoint, "m".into(), "tei".into(), None, DIM, 10)
                    .unwrap();
            let engine = MemoryEngine::builder(DIM).path(db).build().unwrap();
            (Arc::new(engine), Arc::new(provider))
        })
        .await
        .unwrap();
        let tokens = TokenTable::load(&write_token_file(dir.path(), &MACHINES)).unwrap();
        let server = MemoryMcpServer::new(Arc::clone(&engine), Some(provider), None, DIM);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = oneshot::channel();
        let hosts = allowed_hosts.iter().map(|h| (*h).to_owned()).collect();
        let task = tokio::spawn(http::serve(listener, server, tokens, hosts, async move {
            let _ = rx.await;
        }));
        Self {
            url,
            client: reqwest::Client::new(),
            engine,
            embedder,
            dir,
            shutdown: Some(tx),
            task: Some(task),
        }
    }

    /// A request to `path` with the media types rmcp requires.
    pub fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.url))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
    }

    /// A JSON-RPC POST to `/mcp`, with the machine's token when one is given.
    pub fn rpc(
        &self,
        machine: Option<&str>,
        method: &str,
        params: &Value,
    ) -> reqwest::RequestBuilder {
        let request = self
            .request(reqwest::Method::POST, "/mcp")
            .json(&jsonrpc(method, params));
        match machine {
            Some(m) => request.bearer_auth(token(m)),
            None => request,
        }
    }

    /// Call `tool` as `machine`; the raw JSON-RPC response.
    pub async fn call(&self, machine: &str, tool: &str, args: Value) -> Value {
        let response = self
            .rpc(
                Some(machine),
                "tools/call",
                &json!({ "name": tool, "arguments": args }),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response.json().await.unwrap()
    }

    /// Call `tool` as `machine`; the tool's own JSON result.
    pub async fn call_tool(&self, machine: &str, tool: &str, args: Value) -> Value {
        let response = self.call(machine, tool, args).await;
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no tool result in {response}"));
        serde_json::from_str(text).unwrap()
    }

    pub async fn fact_count(&self) -> i64 {
        self.engine.statistics().await.unwrap().facts.total
    }

    #[must_use]
    pub fn snapshot_path(&self) -> PathBuf {
        self.dir.path().join("memory.db.snapshot")
    }

    /// Fire the shutdown signal and wait for `serve` to return.
    pub async fn stop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            tokio::time::timeout(Duration::from_secs(30), task)
                .await
                .expect("serve did not return within 30 s of the shutdown signal")
                .unwrap()
                .unwrap();
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// Write the binary's TOML config for a store in `dir` and return its path.
pub fn write_config(dir: &Path, extra: &str) -> PathBuf {
    let file = dir.join("config.toml");
    let db = dir.join("memory.db");
    std::fs::write(
        &file,
        format!(
            "[engine]\ndb_path = '{}'\nembed_dim = {DIM}\n{extra}",
            db.display()
        ),
    )
    .unwrap();
    file
}

/// The built binary, serving on a loopback port freed just before it starts.
pub struct ServerProcess {
    pub child: Child,
    pub port: u16,
    pub stderr: PathBuf,
}

impl ServerProcess {
    /// Start the binary with `--listen`, `--token-file` and `--config` for `dir`, plus `args`
    /// and `envs`. No inherited `MEMORY_MCP_*` variable reaches it.
    pub fn spawn(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Self {
        Self::spawn_with_token_file(dir, &write_token_file(dir, &MACHINES), args, envs)
    }

    pub fn spawn_with_token_file(
        dir: &Path,
        token_file: &Path,
        args: &[&str],
        envs: &[(&str, &str)],
    ) -> Self {
        let config = dir.join("config.toml");
        if !config.exists() {
            write_config(dir, "");
        }
        let mut with_config = vec!["--config", config.to_str().unwrap()];
        with_config.extend_from_slice(args);
        Self::spawn_bare(dir, token_file, &with_config, envs)
    }

    /// Start the binary with only `--listen` and `--token-file` plus `args` and `envs`: no
    /// `--config`, as a deployment configured entirely through its environment runs it.
    pub fn spawn_bare(dir: &Path, token_file: &Path, args: &[&str], envs: &[(&str, &str)]) -> Self {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let stderr = dir.join("stderr.log");
        let mut command = Command::new(env!("CARGO_BIN_EXE_memory-engine-mcp"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("MEMORY_MCP_") {
                command.env_remove(key);
            }
        }
        command
            .arg("--listen")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--token-file")
            .arg(token_file)
            .args(args)
            .envs(envs.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&stderr).unwrap());
        Self {
            child: command.spawn().unwrap(),
            port,
            stderr,
        }
    }

    #[must_use]
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    #[must_use]
    pub fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    /// Wait until `GET /mcp` answers 401, or return the exit status if the child dies first.
    pub fn wait_ready(&mut self) -> Result<(), ExitStatus> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return Err(status);
            }
            if let Ok(response) = client.get(format!("{}/mcp", self.url())).send()
                && response.status() == 401
            {
                return Ok(());
            }
            assert!(
                Instant::now() < deadline,
                "server not ready in time; stderr:\n{}",
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wait for the child to exit on its own, at most `timeout`.
    pub fn wait_exit(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "server did not exit in time; stderr:\n{}",
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Send SIGTERM and wait at most 10 s, podman's default stop timeout.
    pub fn terminate(&mut self) -> ExitStatus {
        let sent = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(sent.success());
        self.wait_exit(Duration::from_secs(10))
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
