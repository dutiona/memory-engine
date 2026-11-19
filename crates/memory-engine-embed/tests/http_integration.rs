//! wiremock-based end-to-end tests for `HttpEmbeddingProvider`'s public
//! `EmbeddingProvider` contract (single + batch happy paths, the failure each answer
//! maps to, the response-body size cap, and the chunking of an over-length input).
//!
//! `HttpEmbeddingProvider` uses `reqwest::blocking::Client` (internal tokio
//! runtime), so all provider work runs inside `spawn_blocking` to avoid the
//! nested-runtime panic — mirroring `asymmetric_embedding.rs`.
#![allow(clippy::unwrap_used)] // test/bench code: panic-on-unwrap is the intended failure signal (#725)

use memory_engine::ServedArtifact;
use memory_engine::error::{EmbedderError, MemoryError};
use memory_engine::traits::EmbeddingProvider;
use memory_engine_embed::HttpEmbeddingProvider;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Mount a `/v1/embeddings` mock returning the given JSON body with a 200 status.
async fn mount_json(server: &MockServer, body: serde_json::Value) {
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

/// Build a provider pointed at the mock server's `/v1/embeddings`.
fn provider_for(uri: &str, expected_dim: usize) -> HttpEmbeddingProvider {
    HttpEmbeddingProvider::new(
        format!("{uri}/v1/embeddings"),
        "m".into(),
        "tei".into(),
        None,
        expected_dim,
        5,
    )
    .expect("client build should not fail")
}

#[tokio::test]
async fn embed_oversized_response_is_rejected() {
    let server = MockServer::start().await;
    // A body larger than the 32 MiB cap. The cap must reject it rather than
    // buffer the whole thing into memory.
    let oversized = "x".repeat(33 * 1024 * 1024);
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_string(oversized))
        .mount(&server)
        .await;
    let uri = server.uri();

    let result = tokio::task::spawn_blocking(move || provider_for(&uri, 4).embed("hello"))
        .await
        .unwrap();

    let err = result.expect_err("oversized body must be rejected");
    assert!(
        matches!(&err, MemoryError::Internal(m) if m.contains("too large")),
        "expected a too-large internal error, got: {err}"
    );
}

#[tokio::test]
async fn embed_batch_oversized_response_is_rejected() {
    let server = MockServer::start().await;
    let oversized = "x".repeat(33 * 1024 * 1024);
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_string(oversized))
        .mount(&server)
        .await;
    let uri = server.uri();

    let result =
        tokio::task::spawn_blocking(move || provider_for(&uri, 4).embed_batch(&["a", "b"]))
            .await
            .unwrap();

    let err = result.expect_err("oversized body must be rejected");
    assert!(
        matches!(&err, MemoryError::Internal(m) if m.contains("too large")),
        "expected a too-large internal error, got: {err}"
    );
}

#[tokio::test]
async fn embed_openai_format_roundtrip() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        json!({ "data": [{ "index": 0, "embedding": [0.1f32, 0.2, 0.3, 0.4] }] }),
    )
    .await;
    let uri = server.uri();

    let out = tokio::task::spawn_blocking(move || provider_for(&uri, 4).embed("hello"))
        .await
        .unwrap()
        .expect("happy-path embed should succeed");
    assert_eq!(out, vec![0.1f32, 0.2, 0.3, 0.4]);
}

#[tokio::test]
async fn embed_batch_openai_multi_doc_happy_path() {
    let server = MockServer::start().await;
    // Two docs, returned out of index order to exercise the index-reorder path.
    mount_json(
        &server,
        json!({ "data": [
            { "index": 1, "embedding": [0.5f32, 0.6] },
            { "index": 0, "embedding": [0.1f32, 0.2] },
        ] }),
    )
    .await;
    let uri = server.uri();

    let out = tokio::task::spawn_blocking(move || provider_for(&uri, 2).embed_batch(&["a", "b"]))
        .await
        .unwrap()
        .expect("happy-path batch should succeed");
    // Reordered by index: doc 0 first, doc 1 second.
    assert_eq!(out, vec![vec![0.1f32, 0.2], vec![0.5f32, 0.6]]);
}

#[tokio::test]
async fn embed_batch_ollama_multi_doc_happy_path() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        json!({ "embeddings": [[0.1f32, 0.2], [0.3f32, 0.4]] }),
    )
    .await;
    let uri = server.uri();

    let out = tokio::task::spawn_blocking(move || provider_for(&uri, 2).embed_batch(&["a", "b"]))
        .await
        .unwrap()
        .expect("happy-path Ollama batch should succeed");
    assert_eq!(out, vec![vec![0.1f32, 0.2], vec![0.3f32, 0.4]]);
}

#[tokio::test]
async fn embed_http_500_is_failed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream model crashed"))
        .mount(&server)
        .await;
    let uri = server.uri();

    let err = tokio::task::spawn_blocking(move || provider_for(&uri, 4).embed("hello"))
        .await
        .unwrap()
        .expect_err("a 500 must propagate as an error");
    assert!(
        matches!(&err, MemoryError::Embedder(EmbedderError::Failed(m))
            if m.contains("500") && m.contains("upstream model crashed")),
        "expected the 500 status and body in a failed embed, got: {err}"
    );
}

#[tokio::test]
async fn embed_batch_http_500_is_failed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;
    let uri = server.uri();

    let err = tokio::task::spawn_blocking(move || provider_for(&uri, 4).embed_batch(&["a", "b"]))
        .await
        .unwrap()
        .expect_err("a 500 must propagate as an error");
    assert!(
        matches!(&err, MemoryError::Embedder(EmbedderError::Failed(m))
            if m.contains("500") && m.contains("boom")),
        "expected the 500 status and body in a failed embed, got: {err}"
    );
}

/// What a retry can do about `err`, as the embedding queue reads it.
const fn kind(err: &MemoryError) -> &'static str {
    match err {
        MemoryError::Embedder(EmbedderError::Unreachable(_)) => "unreachable",
        MemoryError::Embedder(EmbedderError::KeyRejected(_)) => "key rejected",
        MemoryError::Embedder(EmbedderError::InputRejected(_)) => "input rejected",
        MemoryError::Embedder(EmbedderError::NoAnswer(_)) => "no answer",
        MemoryError::Embedder(EmbedderError::Failed(_)) => "failed",
        _ => "not an embedder error",
    }
}

#[tokio::test]
async fn a_status_maps_to_its_embedder_error() {
    for (status, want) in [
        (400, "input rejected"),
        (401, "key rejected"),
        (403, "key rejected"),
        (404, "failed"),
        (413, "input rejected"),
        (422, "input rejected"),
        (500, "failed"),
        (503, "failed"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(status).set_body_string("refused"))
            .mount(&server)
            .await;
        let uri = server.uri();
        let err = tokio::task::spawn_blocking(move || provider_for(&uri, 4).embed("hello"))
            .await
            .unwrap()
            .expect_err("a non-2xx must be an error");
        assert_eq!(kind(&err), want, "{status}: {err}");
        let text = err.to_string();
        assert!(
            text.contains(&format!("embedding endpoint returned {status}"))
                && text.contains("refused"),
            "{status}: {text}"
        );
    }
}

#[tokio::test]
async fn a_closed_port_is_unreachable() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let uri = format!("http://127.0.0.1:{port}");
    let err = tokio::task::spawn_blocking(move || provider_for(&uri, 4).embed("hello"))
        .await
        .unwrap()
        .expect_err("nothing listens on a closed port");
    assert_eq!(kind(&err), "unreachable", "{err}");
    assert!(
        err.to_string().contains("embedding HTTP request failed"),
        "{err}"
    );
}

#[tokio::test]
async fn an_answer_slower_than_the_timeout_is_no_answer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "data": [{ "embedding": [0.1, 0.2, 0.3, 0.4] }] }))
                .set_delay(std::time::Duration::from_secs(3)),
        )
        .mount(&server)
        .await;
    let uri = server.uri();
    let err = tokio::task::spawn_blocking(move || {
        HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "m".into(),
            "tei".into(),
            None,
            4,
            1,
        )
        .unwrap()
        .embed("hello")
    })
    .await
    .unwrap()
    .expect_err("an answer after the timeout is no answer");
    assert_eq!(kind(&err), "no answer", "{err}");
    assert!(
        err.to_string().contains("embedding HTTP request failed"),
        "{err}"
    );
}

/// A server that reads one whole request, writes `answer` and closes the connection.
fn answers_then_closes(answer: &'static [u8]) -> String {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let uri = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buf = [0; 4096];
        let whole = |r: &[u8]| {
            let text = String::from_utf8_lossy(r);
            text.split_once("\r\n\r\n").is_some_and(|(head, body)| {
                let length = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                body.len() >= length
            })
        };
        while !whole(&request) {
            let n = stream.read(&mut buf).unwrap();
            request.extend_from_slice(&buf[..n]);
        }
        stream.write_all(answer).unwrap();
    });
    uri
}

#[tokio::test]
async fn a_connection_that_breaks_off_is_no_answer() {
    for (answer, says) in [
        (&b""[..], "embedding HTTP request failed"),
        (
            &b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{\"data\""[..],
            "embedding response read failed",
        ),
    ] {
        let uri = answers_then_closes(answer);
        let err = tokio::task::spawn_blocking(move || provider_for(&uri, 4).embed("hello"))
            .await
            .unwrap()
            .expect_err("a connection closed before its answer ends is no answer");
        assert_eq!(kind(&err), "no answer", "{err}");
        assert!(err.to_string().contains(says), "{err}");
    }
}

#[tokio::test]
async fn embed_direct_format_roundtrip() {
    // `{"embedding": [...]}`, the shape some servers answer with.
    let server = MockServer::start().await;
    mount_json(&server, json!({ "embedding": [0.1, 0.2, 0.3, 0.4] })).await;
    let uri = server.uri();
    let emb = tokio::task::spawn_blocking(move || provider_for(&uri, 4).embed("direct"))
        .await
        .unwrap()
        .expect("a direct-format response should parse");
    assert_eq!(emb.len(), 4);
}

#[tokio::test]
async fn embed_sends_the_bearer_api_key_when_configured() {
    // The only request the mock answers carries the configured key.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(wiremock::matchers::header(
            "Authorization",
            "Bearer test-key-123",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "data": [{ "embedding": [0.1, 0.2, 0.3, 0.4] }] })),
        )
        .mount(&server)
        .await;
    let uri = server.uri();
    let emb = tokio::task::spawn_blocking(move || {
        HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "m".into(),
            "tei".into(),
            Some("test-key-123".into()),
            4,
            5,
        )
        .unwrap()
        .embed("auth")
    })
    .await
    .unwrap()
    .expect("the keyed request should match the mock");
    assert_eq!(emb.len(), 4);
}

// --- #1021: an over-length input is chunked by the server's own count and pooled ---

const DIM: usize = 4;

/// One token per whitespace-separated word, plus the EOS the server appends.
fn words(text: &str) -> usize {
    text.split_whitespace().count() + 1
}

/// llama-server's answer body for an input over its context (dutiona/infra#77, recorded).
fn recorded_body(n_prompt_tokens: usize, n_ctx: usize) -> Value {
    json!({ "error": {
        "code": 400,
        "message": format!(
            "request ({n_prompt_tokens} tokens) exceeds the available context size \
             ({n_ctx} tokens), try increasing it"
        ),
        "type": "exceed_context_size_error",
        "n_prompt_tokens": n_prompt_tokens,
        "n_ctx": n_ctx,
    }})
}

/// The inputs of one embed request, whether it sent a string or an array.
fn inputs(request: &Request) -> Vec<String> {
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    match &body["input"] {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().map(|v| v.as_str().unwrap().to_owned()).collect(),
        other => panic!("no input in {other}"),
    }
}

/// A llama-server `/tokenize` that counts as [`words`] does.
async fn mount_tokenize(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/tokenize"))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let n = words(body["content"].as_str().unwrap());
            ResponseTemplate::new(200).set_body_json(json!({ "tokens": vec![0; n] }))
        })
        .mount(server)
        .await;
}

/// An embedder that takes at most `limit` tokens: a request holding a longer input gets the
/// recorded 400, any other gets `vector` of each input.
async fn mount_embed(server: &MockServer, limit: usize, vector: fn(&str) -> Vec<f32>) {
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |request: &Request| {
            let inputs = inputs(request);
            if let Some(n) = inputs.iter().map(|t| words(t)).find(|&n| n > limit) {
                return ResponseTemplate::new(400).set_body_json(recorded_body(n, limit + 1));
            }
            let data: Vec<Value> = inputs
                .iter()
                .enumerate()
                .map(|(i, t)| json!({ "index": i, "embedding": vector(t) }))
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({ "data": data }))
        })
        .mount(server)
        .await;
}

/// A piece's vector: how many of its words are `a`, `b` and `c`, then a constant.
fn letters(text: &str) -> Vec<f32> {
    let n = |w: &str| {
        let n = text.split_whitespace().filter(|x| *x == w).count();
        f32::from(u16::try_from(n).unwrap())
    };
    vec![n("a"), n("b"), n("c"), 1.0]
}

/// Seven `a`, seven `b` and seven `c` words: three pieces of 8 tokens at a limit of 8.
fn abc() -> String {
    [["a"; 7], ["b"; 7], ["c"; 7]].concat().join(" ")
}

/// A provider for `provider` declaring `limit` as the served `max_input_tokens`, if any.
fn limited(uri: &str, provider: &str, limit: Option<usize>) -> HttpEmbeddingProvider {
    let built = HttpEmbeddingProvider::new(
        format!("{uri}/v1/embeddings"),
        "m".into(),
        provider.into(),
        None,
        DIM,
        5,
    )
    .unwrap();
    match limit {
        Some(max_input_tokens) => built.with_served_artifact(ServedArtifact {
            artifact_sha256: "0".repeat(64),
            pooling: "last".into(),
            normalization: "l2".into(),
            max_input_tokens,
        }),
        None => built,
    }
}

/// Run `f` off the async executor: the provider's blocking client owns a runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

/// The requests `server` received at `at`, oldest first.
async fn requests(server: &MockServer, at: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == at)
        .collect()
}

/// Every input the embed requests carried, oldest first.
async fn embedded(server: &MockServer) -> Vec<String> {
    requests(server, "/v1/embeddings")
        .await
        .iter()
        .flat_map(inputs)
        .collect()
}

/// The mean of the vectors weighted by their counts, L2-normalised.
#[allow(clippy::cast_possible_truncation)] // f64 accumulation, f32 like the provider's
fn pooled(pieces: &[(usize, Vec<f32>)]) -> Vec<f32> {
    let mut sum = vec![0.0_f64; pieces[0].1.len()];
    for (weight, vector) in pieces {
        let weight = f64::from(u32::try_from(*weight).unwrap());
        for (s, x) in sum.iter_mut().zip(vector) {
            *s = weight.mul_add(f64::from(*x), *s);
        }
    }
    let norm = sum.iter().map(|x| x * x).sum::<f64>().sqrt();
    sum.iter().map(|x| (x / norm) as f32).collect()
}

fn assert_close(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len(), "{actual:?} vs {expected:?}");
    assert!(
        actual
            .iter()
            .zip(expected)
            .all(|(a, e)| (a - e).abs() < 1e-5),
        "{actual:?} vs {expected:?}"
    );
}

/// `pieces` weighted by the word count, as a `/tokenize` counting [`words`] weighs them.
fn by_words(pieces: &[String], vector: fn(&str) -> Vec<f32>) -> Vec<f32> {
    let weighted: Vec<_> = pieces.iter().map(|p| (words(p), vector(p))).collect();
    pooled(&weighted)
}

#[tokio::test]
async fn over_length_input_is_chunked_by_the_servers_count() {
    let server = MockServer::start().await;
    mount_tokenize(&server).await;
    mount_embed(&server, 8, letters).await;
    let uri = server.uri();
    let text = abc();
    let input = text.clone();
    let vector = blocking(move || {
        HttpEmbeddingProvider::new(
            format!("{uri}/v1/embeddings"),
            "m".into(),
            "llama.cpp".into(),
            Some("test-key".into()),
            DIM,
            5,
        )
        .unwrap()
        .with_served_artifact(ServedArtifact {
            artifact_sha256: "0".repeat(64),
            pooling: "last".into(),
            normalization: "l2".into(),
            max_input_tokens: 8,
        })
        .embed(&input)
    })
    .await
    .unwrap();

    let sent = embedded(&server).await;
    assert_eq!(sent.len(), 3, "{sent:?}");
    assert_eq!(sent.concat(), text);
    assert!(sent.iter().all(|p| words(p) <= 8), "{sent:?}");
    assert_close(&vector, &by_words(&sent, letters));

    let counted = requests(&server, "/tokenize").await;
    let body: Value = serde_json::from_slice(&counted[0].body).unwrap();
    assert_eq!(body["add_special"], true);
    assert_eq!(body["parse_special"], true);
    assert_eq!(
        counted[0].headers.get("authorization").unwrap(),
        "Bearer test-key"
    );
}

#[tokio::test]
async fn over_length_short_input_is_never_counted() {
    let server = MockServer::start().await;
    mount_tokenize(&server).await;
    mount_embed(&server, 64, letters).await;
    let uri = server.uri();
    blocking(move || limited(&uri, "llama.cpp", Some(64)).embed("a short fact"))
        .await
        .unwrap();
    assert!(requests(&server, "/tokenize").await.is_empty());
    assert_eq!(embedded(&server).await, ["a short fact"]);
}

/// The byte bound's count: a token covers at least one byte, plus 2 special tokens.
const fn byte_bound(text: &str) -> usize {
    text.len() + 2
}

/// Assert `vector` pools `text`'s pieces, which the byte bound kept within `limit`.
async fn assert_byte_bound_pieces(server: &MockServer, text: &str, limit: usize, vector: &[f32]) {
    let sent = embedded(server).await;
    assert!(sent.len() > 1, "{sent:?}");
    assert_eq!(sent.concat(), text);
    assert!(sent.iter().all(|p| byte_bound(p) <= limit), "{sent:?}");
    let weighted: Vec<_> = sent.iter().map(|p| (byte_bound(p), letters(p))).collect();
    assert_close(vector, &pooled(&weighted));
}

#[tokio::test]
async fn over_length_without_tokenize_uses_the_byte_bound() {
    let server = MockServer::start().await;
    mount_tokenize(&server).await;
    mount_embed(&server, 16, letters).await;
    let uri = server.uri();
    let vector = blocking(move || limited(&uri, "tei", Some(16)).embed(&abc()))
        .await
        .unwrap();
    assert!(requests(&server, "/tokenize").await.is_empty());
    assert_byte_bound_pieces(&server, &abc(), 16, &vector).await;
}

#[tokio::test]
async fn over_length_missing_tokenize_falls_back_to_the_byte_bound() {
    for answer in [
        ResponseTemplate::new(404),
        ResponseTemplate::new(400),
        ResponseTemplate::new(405),
        ResponseTemplate::new(413),
        ResponseTemplate::new(422),
        ResponseTemplate::new(200).set_body_json(json!({ "count": 3 })),
        ResponseTemplate::new(200).set_body_string("<html>not here</html>"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/tokenize"))
            .respond_with(answer)
            .mount(&server)
            .await;
        mount_embed(&server, 16, letters).await;
        let uri = server.uri();
        let vector = blocking(move || limited(&uri, "llama.cpp", Some(16)).embed(&abc()))
            .await
            .unwrap();
        assert_eq!(requests(&server, "/tokenize").await.len(), 1);
        assert_byte_bound_pieces(&server, &abc(), 16, &vector).await;
    }
}

#[tokio::test]
async fn over_length_without_a_declared_limit_is_chunked_by_the_recorded_answer() {
    let server = MockServer::start().await;
    mount_tokenize(&server).await;
    mount_embed(&server, 8, letters).await;
    let uri = server.uri();
    let vector = blocking(move || limited(&uri, "tei", None).embed(&abc()))
        .await
        .unwrap();
    let sent = embedded(&server).await;
    assert_eq!(sent[0], abc(), "the whole input is tried first");
    let pieces = &sent[1..];
    assert_eq!(pieces.len(), 3, "{sent:?}");
    assert_eq!(pieces.concat(), abc());
    assert!(!requests(&server, "/tokenize").await.is_empty());
    assert_close(&vector, &by_words(pieces, letters));
}

#[tokio::test]
async fn over_length_in_a_batch_is_chunked_alone() {
    let server = MockServer::start().await;
    mount_tokenize(&server).await;
    mount_embed(&server, 8, letters).await;
    let uri = server.uri();
    let vectors = blocking(move || {
        limited(&uri, "llama.cpp", Some(8)).embed_batch(&["a b", &abc(), "c c c"])
    })
    .await
    .unwrap();

    let (whole, alone): (Vec<_>, Vec<_>) = requests(&server, "/v1/embeddings")
        .await
        .iter()
        .map(inputs)
        .partition(|b| b.len() > 1);
    assert_eq!(whole, [["a b", "c c c"]]);
    let pieces: Vec<String> = alone.into_iter().flatten().collect();
    assert_eq!(pieces.concat(), abc());
    assert!(pieces.iter().all(|p| words(p) <= 8), "{pieces:?}");
    assert_eq!(vectors.len(), 3);
    assert_eq!(vectors[0], letters("a b"));
    assert_close(&vectors[1], &by_words(&pieces, letters));
    assert_eq!(vectors[2], letters("c c c"));
}

#[tokio::test]
async fn over_length_recorded_answer_in_a_batch_falls_back_per_text() {
    let server = MockServer::start().await;
    mount_tokenize(&server).await;
    mount_embed(&server, 8, letters).await;
    let uri = server.uri();
    let vectors = blocking(move || limited(&uri, "tei", None).embed_batch(&["a b", &abc()]))
        .await
        .unwrap();

    let batches: Vec<Vec<String>> = requests(&server, "/v1/embeddings")
        .await
        .iter()
        .map(inputs)
        .collect();
    assert_eq!(
        batches[0],
        ["a b".to_owned(), abc()],
        "the batch is tried first"
    );
    assert_eq!(batches[1], ["a b"]);
    assert_eq!(batches[2], [abc()]);
    let pieces: Vec<String> = batches[3..].iter().flatten().cloned().collect();
    assert_eq!(pieces.concat(), abc());
    assert_eq!(vectors.len(), 2);
    assert_eq!(vectors[0], letters("a b"));
    assert_close(&vectors[1], &by_words(&pieces, letters));
}

#[tokio::test]
async fn over_length_tokenize_401_is_key_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/tokenize"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
        .mount(&server)
        .await;
    mount_embed(&server, 8, letters).await;
    let uri = server.uri();
    let err = blocking(move || limited(&uri, "llama.cpp", Some(8)).embed(&abc()))
        .await
        .unwrap_err();
    assert_eq!(kind(&err), "key rejected", "{err}");
    assert_eq!(embedded(&server).await, Vec::<String>::new());
}

#[tokio::test]
async fn over_length_counted_400_after_a_fitting_count_is_input_rejected() {
    // The count says the input fits; the server disagrees.
    for (provider, limit, embeds) in [("llama.cpp", Some(8), 1), ("tei", None, 2)] {
        let server = MockServer::start().await;
        mount_tokenize(&server).await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(400).set_body_json(recorded_body(9, 9)))
            .mount(&server)
            .await;
        let uri = server.uri();
        let err = blocking(move || limited(&uri, provider, limit).embed("alpha beta gamma"))
            .await
            .unwrap_err();
        assert_eq!(kind(&err), "input rejected", "{provider}: {err}");
        assert!(
            err.to_string().contains("exceed_context_size_error"),
            "{err}"
        );
        assert_eq!(embedded(&server).await.len(), embeds, "{provider}");
    }
}

/// The served `max_input_tokens` may be larger than the server's real context: its recorded
/// answer then gives the limit, and the text is split again, once, by it.
#[tokio::test]
async fn over_length_declared_above_the_servers_context_is_split_by_its_answer() {
    // One that fits the declared 16 whole, one whose first 16-token piece is refused.
    for text in [["a"; 12].join(" "), abc()] {
        let server = MockServer::start().await;
        mount_tokenize(&server).await;
        mount_embed(&server, 8, letters).await;
        let uri = server.uri();
        let input = text.clone();
        let vector = blocking(move || limited(&uri, "llama.cpp", Some(16)).embed(&input))
            .await
            .unwrap();
        let (fitting, refused): (Vec<String>, Vec<String>) = embedded(&server)
            .await
            .into_iter()
            .partition(|p| words(p) <= 8);
        assert_eq!(refused.len(), 1, "split again once: {refused:?}");
        assert_eq!(fitting.concat(), text);
        assert_close(&vector, &by_words(&fitting, letters));
    }
}

#[tokio::test]
async fn over_length_declared_above_the_servers_context_in_a_batch_is_split_by_its_answer() {
    let server = MockServer::start().await;
    mount_tokenize(&server).await;
    mount_embed(&server, 8, letters).await;
    let uri = server.uri();
    // Counted to fit the declared 16, the `d` text makes the batch request refused.
    let d = ["d"; 12].join(" ");
    let vectors =
        blocking(move || limited(&uri, "llama.cpp", Some(16)).embed_batch(&["a b", &d, &abc()]))
            .await
            .unwrap();

    assert_eq!(vectors.len(), 3);
    assert_eq!(vectors[0], letters("a b"));
    assert_close(&vectors[1], &[0.0, 0.0, 0.0, 1.0]);
    let abc_pieces: Vec<String> = requests(&server, "/v1/embeddings")
        .await
        .iter()
        .map(inputs)
        .filter(|batch| batch.len() == 1)
        .flatten()
        .filter(|p| words(p) <= 8 && !p.contains('d') && p != "a b")
        .collect();
    assert_eq!(abc_pieces.concat(), abc());
    assert_close(&vectors[2], &by_words(&abc_pieces, letters));
}

#[tokio::test]
async fn over_length_pooling_applies_mrl_per_piece() {
    let server = MockServer::start().await;
    mount_tokenize(&server).await;
    // Truncated to 2 and renormalised, the pieces are [1, 0] and [0, 1]; truncating after
    // pooling would give [1, 3] / √10 instead.
    mount_embed(&server, 8, |t| {
        if t.starts_with('a') {
            vec![1.0, 0.0, 5.0, 5.0]
        } else {
            vec![0.0, 3.0, 5.0, 5.0]
        }
    })
    .await;
    let uri = server.uri();
    let text = [["a"; 7], ["b"; 7]].concat().join(" ");
    let vector = blocking(move || {
        limited(&uri, "llama.cpp", Some(8))
            .with_mrl_dim(2)
            .unwrap()
            .embed(&text)
    })
    .await
    .unwrap();
    assert_eq!(embedded(&server).await.len(), 2);
    assert_close(&vector, &[std::f32::consts::FRAC_1_SQRT_2; 2]);
}

#[tokio::test]
async fn over_length_zero_norm_pool_is_an_error() {
    let server = MockServer::start().await;
    mount_tokenize(&server).await;
    mount_embed(&server, 8, |t| {
        let x = if t.starts_with('a') { 1.0 } else { -1.0 };
        vec![x, 0.0, 0.0, 0.0]
    })
    .await;
    let uri = server.uri();
    let text = [["a"; 7], ["b"; 7]].concat().join(" ");
    let err = blocking(move || limited(&uri, "llama.cpp", Some(8)).embed(&text))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("zero or non-finite L2 norm"),
        "{err}"
    );
}

#[tokio::test]
async fn over_length_rule_leaves_another_400_alone() {
    let mut other_type = recorded_body(9, 9);
    other_type["error"]["type"] = json!("invalid_request_error");
    let mut textual_n_ctx = recorded_body(9, 9);
    textual_n_ctx["error"]["n_ctx"] = json!("9");
    for answer in [
        ResponseTemplate::new(400).set_body_string("bad request"),
        ResponseTemplate::new(400).set_body_json(other_type),
        ResponseTemplate::new(400).set_body_json(textual_n_ctx),
        ResponseTemplate::new(413).set_body_json(recorded_body(9, 9)),
        ResponseTemplate::new(422).set_body_json(recorded_body(9, 9)),
    ] {
        let server = MockServer::start().await;
        mount_tokenize(&server).await;
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(answer)
            .mount(&server)
            .await;
        let uri = server.uri();
        let err = blocking(move || limited(&uri, "tei", None).embed(&abc()))
            .await
            .unwrap_err();
        assert_eq!(kind(&err), "input rejected", "{err}");
        assert_eq!(embedded(&server).await.len(), 1, "{err}");
        assert!(requests(&server, "/tokenize").await.is_empty(), "{err}");
    }
}

// ── Upstream bodies in diagnostics (#1024) ────────────────────────────────────

fn ghp() -> String {
    format!("ghp_{}", "0123456789abcdefghij".repeat(2))
}

/// No 8-byte window of `secret` appears in `text`.
fn holds_no_fragment(text: &str, secret: &str) -> bool {
    secret
        .as_bytes()
        .windows(8)
        .all(|w| !text.as_bytes().windows(8).any(|t| t == w))
}

/// The error `provider_for` answers when every request gets `response`, for one text and
/// for a batch.
async fn errors_for(response: ResponseTemplate) -> [MemoryError; 2] {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(response)
        .mount(&server)
        .await;
    let uri = server.uri();
    tokio::task::spawn_blocking(move || {
        let provider = provider_for(&uri, 4);
        [
            provider.embed("hello").unwrap_err(),
            provider.embed_batch(&["a", "b"]).unwrap_err(),
        ]
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn an_error_body_is_redacted_before_it_is_cut() {
    let token = ghp();
    // The token starts at byte 970 of the body, so the 1,000-byte cut alone would keep 30
    // bytes of it.
    let failed = format!("{} {token} {}", "x".repeat(969), "y".repeat(500));
    // A 2xx body of no known shape is quoted as JSON: `{"note":"` puts the token at byte 970.
    let shapeless = json!({ "note": format!("{} {token} {}", "x".repeat(960), "y".repeat(500)) });
    let mut texts = Vec::new();
    for response in [
        ResponseTemplate::new(500).set_body_string(failed),
        ResponseTemplate::new(200).set_body_json(shapeless),
    ] {
        for err in errors_for(response).await {
            texts.push(err.to_string());
        }
    }
    for text in &texts {
        assert!(
            text.contains("[REDACTED:github-pat]") && holds_no_fragment(text, &token),
            "{text}"
        );
    }

    // The facade stores the provider's text as it is.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("memory.db");
    let engine = memory_engine::MemoryEngine::builder(4)
        .path(&db)
        .build()
        .unwrap();
    engine
        .add_fact_deferred(&memory_engine::types::AddFactRequest {
            content: "plain".into(),
            fact_type: memory_engine::types::FactType::Semantic,
            source_event_id: None,
            scope: None,
            opts: None,
        })
        .await
        .unwrap();
    let memory_engine::NextEmbedding::Due(item) = engine.next_embedding().await.unwrap() else {
        panic!("queued")
    };
    let [error, _] = errors_for(ResponseTemplate::new(500).set_body_string(format!(
        "{} {token} {}",
        "x".repeat(969),
        "y".repeat(500)
    )))
    .await;
    let text = error.to_string();
    let fingerprint = memory_engine::EmbeddingFingerprint::new("tei", "m", 4);
    engine
        .settle_embedding(
            &item,
            Err(error),
            &fingerprint,
            memory_engine::Backoff::DEFAULT,
        )
        .await
        .unwrap();
    let mut engine = engine;
    engine.close().await.unwrap();
    let last = memory_engine::read_embed_queue_status(&db)
        .unwrap()
        .last_error
        .unwrap();
    assert_eq!(last, text, "the facade stores the provider's text as it is");
    assert!(
        last.contains("[REDACTED:github-pat]") && holds_no_fragment(&last, &token),
        "{last}"
    );
}

#[tokio::test]
async fn an_error_body_over_8_kib_is_named_by_its_size_only() {
    // A URL password whose authority ends past 8 KiB: a cut there could hide its `@`.
    let body = format!("see https://u:{}@example.com/", "p".repeat(9000));
    let len = body.len();
    for err in errors_for(ResponseTemplate::new(502).set_body_string(body)).await {
        let text = err.to_string();
        assert!(
            text.contains("502")
                && text.contains(&format!("{len}-byte body"))
                && !text.contains("ppp"),
            "{text}"
        );
    }
}

#[tokio::test]
async fn a_token_behind_a_json_escape_is_redacted() {
    // In the body's JSON text the token follows the `n` of a `\n` escape, a word character, so
    // only the decoded string shows the line break that makes it a machine token.
    let token = format!("me_{}", "Zq7Xw3Rt9Lp2Vb6Nm4Kc8Hd1Fs5Gj0Ay");
    let body = format!(r#"{{"error":{{"message":"bad key\n{token}"}}}}"#);
    for err in errors_for(ResponseTemplate::new(500).set_body_string(body)).await {
        let text = err.to_string();
        assert!(
            text.contains("[REDACTED:machine-token]") && !text.contains(&token),
            "{text}"
        );
    }
}
