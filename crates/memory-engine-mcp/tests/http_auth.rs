//! The Bearer layer in front of `/mcp` (ADR-0020 §3, §4), driven over real HTTP.

mod common;

use common::{TestServer, initialize_params, jsonrpc, token};
use reqwest::Method;
use reqwest::header::{AUTHORIZATION, HOST, WWW_AUTHENTICATE};
use serde_json::{Value, json};

/// The `Authorization` values a refused request carries: `None` sends no header.
fn refused_credentials() -> Vec<Vec<String>> {
    let laptop = token("laptop");
    vec![
        vec![],
        vec!["Bearer not-a-token".to_owned()],
        vec![format!("bearer {laptop}")],
        vec![format!("Bearer {}", token("stranger"))],
        vec![format!("Bearer {laptop}"), format!("Bearer {laptop}")],
        vec![
            format!("Bearer {laptop}"),
            format!("Bearer {}", token("desktop")),
        ],
    ]
}

fn add_fact_call() -> Value {
    json!({ "name": "memory_add_fact", "arguments": { "content": "refused" } })
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_every_request_without_a_valid_token() {
    let server = TestServer::start().await;
    let requests: Vec<(Method, &str, Option<Value>)> = vec![
        (
            Method::POST,
            "/mcp",
            Some(jsonrpc("initialize", &initialize_params())),
        ),
        (
            Method::POST,
            "/mcp",
            Some(jsonrpc("tools/list", &json!({}))),
        ),
        (
            Method::POST,
            "/mcp",
            Some(jsonrpc("tools/call", &add_fact_call())),
        ),
        (Method::GET, "/mcp", None),
        (Method::DELETE, "/mcp", None),
        (Method::GET, "/", None),
        (Method::POST, "/lifecycle/events", Some(json!({}))),
    ];
    for credentials in refused_credentials() {
        for (method, path, body) in &requests {
            let mut request = server.request(method.clone(), path);
            for value in &credentials {
                request = request.header(AUTHORIZATION, value);
            }
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = request.send().await.unwrap();
            assert_eq!(
                response.status(),
                401,
                "{method} {path} with {credentials:?}"
            );
            assert_eq!(response.headers()[WWW_AUTHENTICATE], "Bearer");
        }
    }
    assert_eq!(server.fact_count().await, 0);
    assert!(
        server
            .embedder
            .received_requests()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_valid_token_is_served_statelessly() {
    let server = TestServer::start().await;

    // The very first request is a tool call: no initialize, no session.
    let response = server
        .rpc(
            Some("laptop"),
            "tools/call",
            &json!({ "name": "memory_statistics", "arguments": {} }),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
    assert!(response.headers().get("mcp-session-id").is_none());
    let body: Value = response.json().await.unwrap();
    assert!(body["result"]["content"][0]["text"].is_string(), "{body}");

    let response = server
        .rpc(Some("desktop"), "tools/list", &json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    let tools = body["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "memory_add_fact"));

    let response = server
        .rpc(Some("laptop"), "initialize", &initialize_params())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["result"]["serverInfo"]["name"], "memory-engine-mcp");

    let response = server
        .rpc(Some("laptop"), "tools/list", &json!({}))
        .header("mcp-session-id", "stale-session")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn get_and_delete_answer_405_with_a_token() {
    let server = TestServer::start().await;
    for method in [Method::GET, Method::DELETE] {
        let response = server
            .request(method.clone(), "/mcp")
            .bearer_auth(token("laptop"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 405, "{method}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_path_is_404_with_a_token() {
    let server = TestServer::start().await;
    let response = server
        .request(Method::GET, "/")
        .bearer_auth(token("laptop"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
}

async fn status_for_host(server: &TestServer, machine: Option<&str>, host: &str) -> u16 {
    server
        .rpc(machine, "tools/list", &json!({}))
        .header(HOST, host)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_host_check_runs_after_authentication() {
    let server = TestServer::start().await;
    assert_eq!(
        status_for_host(&server, Some("laptop"), "other.example").await,
        403
    );
    assert_eq!(status_for_host(&server, None, "other.example").await, 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_configured_host_is_served() {
    let server = TestServer::start_with_hosts(&["memory.example"]).await;
    for host in ["memory.example", "memory.example:8443", "127.0.0.1"] {
        assert_eq!(
            status_for_host(&server, Some("laptop"), host).await,
            200,
            "{host}"
        );
    }
    assert_eq!(
        status_for_host(&server, Some("laptop"), "other.example").await,
        403
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_configured_host_admits_only_loopback() {
    let server = TestServer::start().await;
    assert_eq!(
        status_for_host(&server, Some("laptop"), "memory.example").await,
        403
    );
    assert_eq!(
        status_for_host(&server, Some("laptop"), "127.0.0.1").await,
        200
    );
}
