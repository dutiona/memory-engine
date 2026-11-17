//! The shared HTTP harness: the real `http::serve` in-process, with a wiremock embedder,
//! and a runner for the built binary. Tests drive both with plain `reqwest`, never with
//! rmcp's client, which `deny.toml` bans.
#![allow(dead_code, reason = "each test binary uses part of the harness")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use memory_engine::{Backoff, MemoryEngine, NextEmbedding, ServedArtifact};
use memory_engine_mcp::auth::TokenTable;
use memory_engine_mcp::embedding::HttpEmbeddingProvider;
use memory_engine_mcp::home::User;
use memory_engine_mcp::http;
use memory_engine_mcp::server::MemoryMcpServer;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

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

/// The mock embedder's answer: one `DIM`-wide vector.
#[must_use]
pub fn an_embedding() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_json(json!({ "data": [{ "embedding": vec![0.25_f32; DIM] }] }))
}

/// The drain's retry schedule in these tests: an outage longer than the cap lasts a fraction
/// of a second.
pub const BACKOFF: Backoff = Backoff {
    base: Duration::from_millis(10),
    cap: Duration::from_millis(100),
};

/// What a [`TestServer`] starts with; the default is the mock embedder behind provider `tei`.
#[derive(Default)]
struct Setup {
    allowed_hosts: Vec<String>,
    without_embedder: bool,
    instruction: Option<String>,
    /// The embedder's port is reserved and closed until [`TestServer::bring_embedder_up`].
    unreachable: bool,
    /// The provider's timeout; 10 s when unset.
    timeout_secs: Option<u64>,
    /// Provider `llama.cpp`, declaring this `max_input_tokens`, beside a `/tokenize`.
    input_limit: Option<usize>,
}

/// llama-server's `/tokenize`, counting one token per two bytes of `content`.
struct HalfByteTokens;

impl Respond for HalfByteTokens {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let tokens = body["content"].as_str().unwrap().len() / 2;
        ResponseTemplate::new(200).set_body_json(json!({ "tokens": vec![0; tokens] }))
    }
}

/// The in-process server, on an ephemeral loopback port.
pub struct TestServer {
    pub url: String,
    pub client: reqwest::Client,
    pub engine: Arc<MemoryEngine>,
    pub provider: Option<Arc<HttpEmbeddingProvider>>,
    pub embedder: MockServer,
    pub dir: TempDir,
    allowed_hosts: Vec<String>,
    closed_port: Option<u16>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<std::io::Result<()>>>,
}

impl TestServer {
    pub async fn start() -> Self {
        Self::start_with(Setup::default()).await
    }

    pub async fn start_with_hosts(allowed_hosts: &[&str]) -> Self {
        Self::start_with(Setup {
            allowed_hosts: allowed_hosts.iter().map(|h| (*h).to_owned()).collect(),
            ..Setup::default()
        })
        .await
    }

    /// A server with no embedder: recall is full-text only and nothing is embedded.
    pub async fn start_without_embedder() -> Self {
        Self::start_with(Setup {
            without_embedder: true,
            ..Setup::default()
        })
        .await
    }

    /// A server whose provider carries `instruction` as its configured query instruction.
    pub async fn start_with_query_instruction(instruction: &str) -> Self {
        Self::start_with(Setup {
            instruction: Some(instruction.to_owned()),
            ..Setup::default()
        })
        .await
    }

    /// A server whose embedder's port is closed: no request reaches it until
    /// [`bring_embedder_up`](Self::bring_embedder_up).
    pub async fn start_unreachable() -> Self {
        Self::start_with(Setup {
            unreachable: true,
            ..Setup::default()
        })
        .await
    }

    /// A server whose provider gives up on an answer after `timeout_secs`.
    pub async fn start_with_timeout(timeout_secs: u64) -> Self {
        Self::start_with(Setup {
            timeout_secs: Some(timeout_secs),
            ..Setup::default()
        })
        .await
    }

    /// A server whose provider is `llama.cpp` declaring `max_input_tokens`, whose `/tokenize`
    /// counts one token per two bytes.
    pub async fn start_with_input_limit(max_input_tokens: usize) -> Self {
        Self::start_with(Setup {
            input_limit: Some(max_input_tokens),
            ..Setup::default()
        })
        .await
    }

    async fn start_with(setup: Setup) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let embedder = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(an_embedding())
            .mount(&embedder)
            .await;
        if setup.input_limit.is_some() {
            Mock::given(method("POST"))
                .and(path("/tokenize"))
                .respond_with(HalfByteTokens)
                .mount(&embedder)
                .await;
        }
        let closed_port = setup.unreachable.then(|| {
            std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port()
        });
        let endpoint = closed_port.map_or_else(
            || format!("{}/v1/embeddings", embedder.uri()),
            |port| format!("http://127.0.0.1:{port}/v1/embeddings"),
        );
        let db = dir.path().join("memory.db");
        let Setup {
            allowed_hosts,
            without_embedder,
            instruction,
            timeout_secs,
            input_limit,
            ..
        } = setup;
        // The blocking reqwest client inside the provider must not be built on the runtime.
        let (engine, provider) = tokio::task::spawn_blocking(move || {
            let provider = (!without_embedder).then(|| {
                let label = if input_limit.is_some() {
                    "llama.cpp"
                } else {
                    "tei"
                };
                let mut provider = HttpEmbeddingProvider::new(
                    endpoint,
                    "m".into(),
                    label.into(),
                    None,
                    DIM,
                    timeout_secs.unwrap_or(10),
                )
                .unwrap();
                if let Some(i) = instruction {
                    provider = provider.with_query_instruction(i);
                }
                if let Some(max_input_tokens) = input_limit {
                    provider = provider.with_served_artifact(ServedArtifact {
                        artifact_sha256: "0".repeat(64),
                        pooling: "last".into(),
                        normalization: "l2".into(),
                        max_input_tokens,
                    });
                }
                Arc::new(provider)
            });
            let engine = MemoryEngine::builder(DIM).path(db).build().unwrap();
            (Arc::new(engine), provider)
        })
        .await
        .unwrap();
        let mut server = Self {
            url: String::new(),
            client: reqwest::Client::new(),
            engine,
            provider,
            embedder,
            dir,
            allowed_hosts,
            closed_port,
            shutdown: None,
            task: None,
        };
        server.serve().await;
        server
    }

    /// Run `http::serve` over the engine on a fresh loopback port.
    async fn serve(&mut self) {
        let tokens = TokenTable::load(&write_token_file(self.dir.path(), &MACHINES)).unwrap();
        let server = MemoryMcpServer::new(
            Arc::clone(&self.engine),
            self.provider.clone(),
            User::parse("alice").unwrap(),
        )
        .with_embed_backoff(BACKOFF);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        self.url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = oneshot::channel();
        let hosts = self.allowed_hosts.clone();
        self.task = Some(tokio::spawn(http::serve(
            listener,
            server,
            tokens,
            hosts,
            async move {
                let _ = rx.await;
            },
        )));
        self.shutdown = Some(tx);
    }

    /// Stop the server, then serve the same store again from a fresh engine.
    pub async fn restart(&mut self) {
        self.stop().await;
        let db = self.db();
        let engine =
            tokio::task::spawn_blocking(move || MemoryEngine::builder(DIM).path(db).build())
                .await
                .unwrap()
                .unwrap();
        self.engine = Arc::new(engine);
        self.serve().await;
    }

    /// Open the closed port of [`start_unreachable`](Self::start_unreachable): a mock embedder
    /// answering every embed with a vector listens there from now on.
    pub async fn bring_embedder_up(&mut self) {
        let port = self
            .closed_port
            .expect("the server was started unreachable");
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        self.embedder = MockServer::builder().listener(listener).start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(an_embedding())
            .mount(&self.embedder)
            .await;
    }

    #[must_use]
    pub fn db(&self) -> PathBuf {
        self.dir.path().join("memory.db")
    }

    /// Fact `id`'s embedding-queue row, `(attempts, next_attempt_at, failed_at, last_error)`;
    /// `None` once it left the queue.
    pub fn queue(&self, id: i64) -> Option<(i64, String, Option<String>, Option<String>)> {
        use rusqlite::OptionalExtension as _;
        rusqlite::Connection::open(self.db())
            .unwrap()
            .query_row(
                "SELECT attempts, next_attempt_at, failed_at, last_error \
                 FROM embed_queue WHERE fact_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .unwrap()
    }

    /// Whether fact `id` has its `DIM`-wide vector.
    pub async fn has_vector(&self, id: i64) -> bool {
        self.engine.get_fact(id).await.unwrap().embedding.len() == DIM
    }

    /// Wait, at most 10 s, until no fact waits for its vector (a failed one aside).
    pub async fn wait_drained(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let next = self.engine.next_embedding().await.unwrap();
            if next == NextEmbedding::Idle {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the queue did not drain in 10 s: {next:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
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
        self.call_with(machine, &[], tool, args).await
    }

    /// Call `tool` as `machine`, sending `facts` as `ME-Project-*` headers: raw bytes, so a
    /// test can send what a client must not; a repeated name is appended.
    pub async fn call_with(
        &self,
        machine: &str,
        facts: &[(&str, &[u8])],
        tool: &str,
        args: Value,
    ) -> Value {
        let mut request = self.rpc(
            Some(machine),
            "tools/call",
            &json!({ "name": tool, "arguments": args }),
        );
        for (name, value) in facts {
            request = request.header(
                *name,
                reqwest::header::HeaderValue::from_bytes(value).unwrap(),
            );
        }
        let response = request.send().await.unwrap();
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

    /// Call a model tool as `machine`: asserts the call line and splits the answer.
    pub async fn tool(&self, machine: &str, name: &str, args: Value) -> Reply {
        self.tool_with(machine, &[], name, args).await
    }

    /// [`Self::tool`] with `ME-Project-*` facts, as [`Self::call_with`] sends them.
    pub async fn tool_with(
        &self,
        machine: &str,
        facts: &[(&str, &[u8])],
        name: &str,
        args: Value,
    ) -> Reply {
        let response = self.call_with(machine, facts, name, args).await;
        assert!(
            response.get("error").is_none(),
            "a model tool answered a JSON-RPC error: {response}"
        );
        let result = &response["result"];
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text content in {response}"));
        let (line, body) = text
            .split_once('\n')
            .unwrap_or_else(|| panic!("no call line in {text:?}"));
        let call = line
            .strip_prefix("call ")
            .unwrap_or_else(|| panic!("the first line is not a call line: {line:?}"));
        assert!(
            call.len() == 16 && call.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "not 16 lowercase hex digits: {call:?}"
        );
        Reply {
            is_error: result["isError"].as_bool().unwrap_or(false),
            call: call.to_owned(),
            body: body.to_owned(),
        }
    }

    /// Store a fact at the scope path `home` (root when `None`) without the tools, as another
    /// writer would; it carries `metadata` and the mock embedder's vector.
    pub async fn seed(&self, home: Option<&str>, content: &str, metadata: Value) -> i64 {
        use memory_engine::traits::EmbeddingProvider;
        let fingerprint = self.provider.as_ref().map_or_else(
            || memory_engine::EmbeddingFingerprint::new("tei", "m", DIM),
            |p| p.fingerprint(),
        );
        self.engine
            .add_fact_precomputed(
                &memory_engine::types::AddFactRequest {
                    content: content.to_owned(),
                    fact_type: memory_engine::types::FactType::Semantic,
                    source_event_id: None,
                    scope: home.map(str::to_owned),
                    opts: Some(memory_engine::types::AddFactOptions {
                        metadata: Some(metadata),
                        ..Default::default()
                    }),
                },
                vec![0.25; DIM],
                &fingerprint,
                None,
            )
            .await
            .unwrap()
    }

    /// The `MemoryOp` records, oldest first: (id, payload, scope path).
    pub fn records(&self) -> Vec<(i64, Value, String)> {
        let conn = rusqlite::Connection::open(self.db()).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT e.id, e.payload, e.scope_id FROM events e \
                 WHERE e.event_type = 'MemoryOp' ORDER BY e.id",
            )
            .unwrap();
        let rows: Vec<(i64, String, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        rows.into_iter()
            .map(|(id, payload, scope)| {
                (
                    id,
                    serde_json::from_str(&payload).unwrap(),
                    scope_path(&conn, scope),
                )
            })
            .collect()
    }

    /// Make every later event insert fail, as a storage fault would.
    pub fn refuse_event_writes(&self) {
        self.sql(
            "CREATE TRIGGER refuse_events BEFORE INSERT ON events \
             BEGIN SELECT RAISE(ABORT, 'event writes refused by the test'); END;",
        );
    }

    /// Run `sql` on the store through a second connection.
    pub fn sql(&self, sql: &str) {
        rusqlite::Connection::open(self.db())
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }

    /// Answer query embeds (their body holds the query instruction) with `query`, and every
    /// other embed with `document`, in place of the default mock.
    pub async fn mount_embedder(
        &self,
        query: impl Respond + 'static,
        document: impl Respond + 'static,
    ) {
        self.embedder.reset().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .and(body_string_contains("Instruct:"))
            .respond_with(query)
            .with_priority(1)
            .mount(&self.embedder)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(document)
            .mount(&self.embedder)
            .await;
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

/// A model tool's answer, its call line checked and split off.
#[derive(Debug)]
pub struct Reply {
    pub is_error: bool,
    pub call: String,
    pub body: String,
}

impl Reply {
    #[must_use]
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
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
    /// The user is `alice` unless `envs` sets `MEMORY_MCP_USER`.
    pub fn spawn_bare(dir: &Path, token_file: &Path, args: &[&str], envs: &[(&str, &str)]) -> Self {
        Self::spawn_inner(dir, token_file, args, envs, true)
    }

    /// As [`spawn_bare`](Self::spawn_bare), with no user setting at all.
    pub fn spawn_without_user(dir: &Path, args: &[&str]) -> Self {
        let tokens = write_token_file(dir, &MACHINES);
        Self::spawn_inner(dir, &tokens, args, &[], false)
    }

    fn spawn_inner(
        dir: &Path,
        token_file: &Path,
        args: &[&str],
        envs: &[(&str, &str)],
        with_user: bool,
    ) -> Self {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        // Per port, so two servers started in one directory keep their own logs.
        let stderr = dir.join(format!("stderr-{port}.log"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_memory-engine-mcp"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("MEMORY_MCP_") {
                command.env_remove(key);
            }
        }
        if with_user {
            command.env("MEMORY_MCP_USER", "alice");
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

/// The full scope path of scope `id`, root excluded, as `user:alice/project:x`.
fn scope_path(conn: &rusqlite::Connection, id: i64) -> String {
    let mut labels = Vec::new();
    let mut current = Some(id);
    while let Some(node) = current {
        let (label, parent): (String, Option<i64>) = conn
            .query_row(
                "SELECT label, parent_id FROM scopes WHERE id = ?1",
                [node],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        if parent.is_none() {
            break;
        }
        labels.push(label);
        current = parent;
    }
    labels.reverse();
    labels.join("/")
}
