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
    /// The store's directory is made inside this one, which a test picks to sit on a disk.
    store_in: Option<PathBuf>,
    /// The author denylist the backfill route redacts with; `None` refuses every upload.
    denylist: Option<Vec<String>>,
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

/// AC2's new prefixes, with the rule each placeholder names (#1024).
pub const AC2: [(&str, &str); 16] = [
    ("hf_", "huggingface-token"),
    ("tskey-", "tailscale-key"),
    ("glpat-", "gitlab-pat"),
    ("gho_", "github-token"),
    ("ghu_", "github-token"),
    ("ghs_", "github-token"),
    ("ghr_", "github-token"),
    ("sk_live_", "stripe-key"),
    ("rk_live_", "stripe-key"),
    ("npm_", "npm-token"),
    ("pypi-", "pypi-token"),
    ("AGE-SECRET-KEY-1", "age-secret-key"),
    ("xapp-", "slack-token"),
    ("sbp_", "supabase-token"),
    ("dckr_pat_", "docker-pat"),
    ("dop_v1_", "digitalocean-token"),
];

/// A synthetic token for `prefix`, distinct per `i`.
#[must_use]
pub fn ac2_token(i: usize, prefix: &str) -> String {
    format!("{prefix}{i:02}Zq7Xw3Rt9Lp2Vb6Nm4Kc8Hd")
}

/// A synthetic machine-token-shaped value.
#[must_use]
pub fn machine_token() -> String {
    format!("me_{}", "Zq7Xw3Rt9Lp2Vb6Nm4Kc8Hd1Fs5Gj0Ay")
}

#[must_use]
pub fn ghp() -> String {
    format!("ghp_{}", "0123456789abcdefghij".repeat(2))
}

/// No 8-byte window of `secret` appears in `text`.
#[must_use]
pub fn holds_no_fragment(text: &str, secret: &str) -> bool {
    secret
        .as_bytes()
        .windows(8)
        .all(|w| !holds(text.as_bytes(), w))
}

/// The authenticated machine `name`, as the Bearer layer would produce it.
#[must_use]
pub fn machine(name: &str) -> memory_engine_mcp::auth::Machine {
    let dir = tempfile::tempdir().unwrap();
    let table = TokenTable::load(&write_token_file(dir.path(), &MACHINES)).unwrap();
    table
        .authenticate(format!("Bearer {}", token(name)).as_bytes())
        .unwrap()
        .clone()
}

/// One checked-in hook fixture: the headers the client sent and the hook input, as the outbox
/// stores them. Each builder method changes a copy, so a test overrides what it needs.
#[derive(Clone)]
pub struct Fixture {
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// The names of the checked-in fixtures, sorted.
#[must_use]
pub fn fixture_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .map(Result::unwrap)
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-code-hooks")
}

/// The fixture `name`.
#[must_use]
pub fn fixture(name: &str) -> Fixture {
    let dir = fixtures_dir().join(name);
    let headers = std::fs::read_to_string(dir.join("headers")).unwrap();
    Fixture {
        headers: headers
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                let (name, value) = l.split_once(": ").unwrap();
                (name.to_owned(), value.to_owned())
            })
            .collect(),
        body: std::fs::read(dir.join("body")).unwrap(),
    }
}

impl Fixture {
    /// Set header `name`, replacing any value.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    #[must_use]
    pub fn without(mut self, name: &str) -> Self {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        self
    }

    #[must_use]
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// Change the hook input's JSON.
    #[must_use]
    pub fn json(mut self, edit: impl FnOnce(&mut Value)) -> Self {
        let mut value: Value = serde_json::from_slice(&self.body).unwrap();
        edit(&mut value);
        self.body = serde_json::to_vec(&value).unwrap();
        self
    }

    /// The hook input as JSON.
    #[must_use]
    pub fn json_value(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }

    /// The event ID header.
    #[must_use]
    pub fn id(&self) -> &str {
        &self
            .headers
            .iter()
            .find(|(n, _)| n == "ME-Event-Id")
            .unwrap()
            .1
    }

    /// The headers as a map, as the route reads them.
    #[must_use]
    pub fn header_map(&self) -> axum::http::HeaderMap {
        let mut map = axum::http::HeaderMap::new();
        for (name, value) in &self.headers {
            map.append(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }
}

/// What the route answered one event with.
#[derive(Debug, PartialEq, Eq)]
pub struct Posted {
    pub status: u16,
    pub outcome: Option<String>,
}

impl Posted {
    #[must_use]
    pub fn is(&self, status: u16, outcome: Option<&str>) -> bool {
        self.status == status && self.outcome.as_deref() == outcome
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
    denylist: Option<Vec<String>>,
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

    /// A server with no embedder whose store lives in a new directory inside `parent`.
    pub async fn start_without_embedder_in(parent: &Path) -> Self {
        Self::start_with(Setup {
            without_embedder: true,
            store_in: Some(parent.to_owned()),
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

    /// A server whose backfill route redacts with `literals` (an empty list is an explicit
    /// "none"), injected as `main` would load `ME_REDACT_DENYLIST_FILE`.
    pub async fn start_with_denylist(literals: &[&str]) -> Self {
        Self::start_with(Setup {
            denylist: Some(literals.iter().map(|l| (*l).to_owned()).collect()),
            ..Setup::default()
        })
        .await
    }

    /// [`start_with_denylist`](Self::start_with_denylist), with the embedder's port closed.
    pub async fn start_unreachable_with_denylist(literals: &[&str]) -> Self {
        Self::start_with(Setup {
            unreachable: true,
            denylist: Some(literals.iter().map(|l| (*l).to_owned()).collect()),
            ..Setup::default()
        })
        .await
    }

    /// [`start_with_denylist`](Self::start_with_denylist), its store in a new directory inside
    /// `parent`.
    pub async fn start_with_denylist_in(literals: &[&str], parent: &Path) -> Self {
        Self::start_with(Setup {
            denylist: Some(literals.iter().map(|l| (*l).to_owned()).collect()),
            store_in: Some(parent.to_owned()),
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
        let dir = setup.store_in.as_deref().map_or_else(
            || tempfile::tempdir().unwrap(),
            |parent| tempfile::tempdir_in(parent).unwrap(),
        );
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
            denylist,
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
            denylist,
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
        let mut server = MemoryMcpServer::new(
            Arc::clone(&self.engine),
            self.provider.clone(),
            User::parse("alice").unwrap(),
        )
        .with_embed_backoff(BACKOFF);
        if let Some(denylist) = &self.denylist {
            server = server.with_denylist(denylist.clone());
        }
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

    /// Stop the server, then serve the same store again under `denylist`.
    pub async fn restart_with_denylist(&mut self, denylist: Option<Vec<String>>) {
        self.denylist = denylist;
        self.restart().await;
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

    /// Post `fixture` to `/lifecycle/events` as `machine`.
    pub async fn post_event(&self, machine: &str, fixture: &Fixture) -> Posted {
        let mut request = self
            .client
            .post(format!("{}/lifecycle/events", self.url))
            .bearer_auth(token(machine))
            .body(fixture.body.clone());
        for (name, value) in &fixture.headers {
            request = request.header(name, value);
        }
        let response = request.send().await.unwrap();
        Posted {
            status: response.status().as_u16(),
            outcome: response
                .headers()
                .get("ME-Event-Outcome")
                .map(|v| v.to_str().unwrap().to_owned()),
        }
    }

    /// The status of a `/lifecycle/events` request whose headers are `fixture`'s and whose body
    /// is claimed to be `claimed` bytes long and never sent: what the server decides from the
    /// headers alone.
    pub async fn status_from_headers(&self, machine: &str, fixture: &Fixture, claimed: u64) -> u16 {
        use std::fmt::Write as _;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(self.url.trim_start_matches("http://"))
            .await
            .unwrap();
        let mut head = format!(
            "POST /lifecycle/events HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Length: {claimed}\r\n",
            self.url.trim_start_matches("http://"),
            token(machine)
        );
        for (name, value) in &fixture.headers {
            write!(head, "{name}: {value}\r\n").unwrap();
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).await.unwrap();
        let mut answer = vec![0; 12];
        stream.read_exact(&mut answer).await.unwrap();
        String::from_utf8(answer[9..12].to_vec())
            .unwrap()
            .parse()
            .unwrap()
    }

    /// One number from the store, through a second connection.
    pub fn number(&self, sql: &str) -> i64 {
        rusqlite::Connection::open(self.db())
            .unwrap()
            .query_row(sql, [], |r| r.get(0))
            .unwrap()
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

/// Every file of the server's directory, as bytes.
pub fn store_bytes(server: &TestServer) -> Vec<(String, Vec<u8>)> {
    std::fs::read_dir(server.dir.path())
        .unwrap()
        .map(Result::unwrap)
        .filter(|e| e.file_type().unwrap().is_file())
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

#[must_use]
pub fn holds(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
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

/// The log line `main` writes right after binding its listener; [`ServerProcess::wait_ready`]
/// reads the bound port from it.
pub const LISTENING: &str = "listening on";

/// The built binary, serving on a loopback port it picks itself and logs, known once
/// [`wait_ready`](Self::wait_ready) has returned `Ok`.
///
/// Readiness reads the child's own [`LISTENING`] line, which is logged at `info`: a test that sets
/// `RUST_LOG` must keep `memory_engine_mcp` at `info` or finer, and must name the crate, since
/// `main` replaces a bare level with WARN (#1149).
pub struct ServerProcess {
    pub child: Child,
    port: Option<u16>,
    pub stderr: PathBuf,
}

impl ServerProcess {
    /// Start the binary with `--listen`, `--token-file` and `--config` for `dir`, plus `args`
    /// and `envs`. No inherited `MEMORY_MCP_*` variable reaches it; `RUST_LOG` is
    /// `memory_engine_mcp=info` unless `envs` sets it (see [`ServerProcess`]).
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
        // Numbered, so two servers started in one directory keep their own logs.
        static SPAWNED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SPAWNED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stderr = dir.join(format!("stderr-{n}.log"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_memory-engine-mcp"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("MEMORY_MCP_") {
                command.env_remove(key);
            }
        }
        if with_user {
            command.env("MEMORY_MCP_USER", "alice");
        }
        // Port 0: a port freed here for the child could be taken by another test's socket before
        // the child binds it, and that test's server would then answer for this one (#1143).
        command
            .env("RUST_LOG", "memory_engine_mcp=info")
            .arg("--listen")
            .arg("127.0.0.1:0")
            .arg("--token-file")
            .arg(token_file)
            .args(args)
            .envs(envs.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&stderr).unwrap());
        Self {
            child: command.spawn().unwrap(),
            port: None,
            stderr,
        }
    }

    /// The port the child bound.
    ///
    /// # Panics
    ///
    /// Before [`wait_ready`](Self::wait_ready) has read it.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
            .expect("call wait_ready first: the child picks its own port")
    }

    #[must_use]
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port())
    }

    /// The port in the first complete [`LISTENING`] line of the child's own log.
    fn logged_port(&self) -> Option<u16> {
        let log = self.stderr();
        let (complete, _) = log.rsplit_once('\n')?;
        complete.lines().find_map(|line| {
            let (_, addr) = line.split_once(LISTENING)?;
            addr.trim()
                .parse::<std::net::SocketAddr>()
                .ok()
                .map(|a| a.port())
        })
    }

    #[must_use]
    pub fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    /// Wait until the child has logged the port it bound and `GET /mcp` there answers 401, or
    /// return the exit status if the child dies first.
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
            if self.port.is_none() {
                self.port = self.logged_port();
            }
            if self.port.is_some()
                && let Ok(response) = client.get(format!("{}/mcp", self.url())).send()
                && response.status() == 401
            {
                return Ok(());
            }
            assert!(
                Instant::now() < deadline,
                "server not ready in time{}; stderr:\n{}",
                if self.port.is_none() {
                    ": no `listening on` line (a test's RUST_LOG must keep memory_engine_mcp at info)"
                } else {
                    ""
                },
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Call the model tool `name` as `machine`; the raw JSON-RPC response.
    pub fn tool(&self, machine: &str, name: &str, args: &Value) -> Value {
        let response = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap()
            .post(format!("{}/mcp", self.url()))
            .bearer_auth(token(machine))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .body(jsonrpc("tools/call", &json!({ "name": name, "arguments": args })).to_string())
            .send()
            .unwrap();
        assert_eq!(response.status(), 200);
        response.json().unwrap()
    }

    /// Post `fixture` to `/lifecycle/events` as `machine`.
    pub fn post_event(&self, machine: &str, fixture: &Fixture) -> Posted {
        let mut request = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap()
            .post(format!("{}/lifecycle/events", self.url()))
            .bearer_auth(token(machine))
            .body(fixture.body.clone());
        for (name, value) in &fixture.headers {
            request = request.header(name, value);
        }
        let response = request.send().unwrap();
        Posted {
            status: response.status().as_u16(),
            outcome: response
                .headers()
                .get("ME-Event-Outcome")
                .map(|v| v.to_str().unwrap().to_owned()),
        }
    }

    /// The status of a `/lifecycle/events` request whose headers are `fixture`'s and whose body
    /// is claimed to be `claimed` bytes long and never sent: what the server decides from the
    /// headers alone.
    pub fn status_from_headers(&self, machine: &str, fixture: &Fixture, claimed: u64) -> u16 {
        use std::fmt::Write as _;
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", self.port())).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut head = format!(
            "POST /lifecycle/events HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Length: {claimed}\r\n",
            token(machine)
        );
        for (name, value) in &fixture.headers {
            write!(head, "{name}: {value}\r\n").unwrap();
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).unwrap();
        let mut answer = [0; 12];
        stream.read_exact(&mut answer).unwrap();
        String::from_utf8(answer[9..12].to_vec())
            .unwrap()
            .parse()
            .unwrap()
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
