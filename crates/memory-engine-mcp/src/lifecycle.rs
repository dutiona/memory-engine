//! `POST /lifecycle/events` (ADR-0020 §3, §5): record one harness event exactly once.
//!
//! The route checks in the ADR's order: the envelope (400), the size (413), a stalled body
//! (408), that it knows the harness (422), the key (a duplicate, or 409), then it parses the body
//! (quarantine) and maps and applies it (422). A 5xx is only ever the answer to a fault that
//! does not depend on the event: the store cannot take a write now, or the server is stopping.
//! The same request then works later.

use std::future::poll_fn;
use std::pin::pin;
use std::time::Duration;

use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{Extension, Request, State};
use axum::http::header::CONTENT_LENGTH;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, NaiveDateTime, Timelike, Utc};
use memory_engine::error::{MemoryError, StorageError};
use memory_engine::types::lifecycle::{Delivery, Recorded};
use sha2::{Digest, Sha256};

use crate::adapters::claude_code;
use crate::auth::Machine;
use crate::home::Home;
use crate::project::ProjectFacts;
use crate::server::MemoryMcpServer;

/// The body limit, 16 MiB (ADR-0020 §3): the engine enforces the same bound on what it applies.
pub const BODY_LIMIT: usize = memory_engine::LIFECYCLE_BODY_BYTES;

/// How long the body may go without a byte before the answer is 408.
pub const BODY_STALL: Duration = Duration::from_secs(60);

/// The most bytes an envelope header may hold.
const MAX_HEADER_BYTES: usize = 4096;

/// The response header that says what a 204 did.
static OUTCOME: HeaderName = HeaderName::from_static("me-event-outcome");

/// The envelope of one event: the neutral headers around the harness's own bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// `ME-Event-Id`: unique per machine.
    pub event_id: String,
    /// `ME-Event-Time`: the client's clock, second resolution.
    pub time: DateTime<Utc>,
    /// `ME-Event-Seq`: the tie-break within one second.
    pub seq: u32,
    /// `ME-Harness`: selects the adapter.
    pub harness: String,
    /// `ME-Client-Version`: the plugin's version, stored as sent.
    pub client_version: String,
}

impl Envelope {
    /// Read the five required headers, each exactly once, within 4,096 bytes and their grammar
    /// (ADR-0020 §3).
    ///
    /// # Errors
    ///
    /// The name of the first header that is missing, repeated, too long or off its grammar.
    ///
    /// # Examples
    ///
    /// ```
    /// use axum::http::HeaderMap;
    /// use memory_engine_mcp::lifecycle::Envelope;
    ///
    /// let mut headers = HeaderMap::new();
    /// for (name, value) in [
    ///     ("ME-Event-Id", "1795282815-000001-4242"),
    ///     ("ME-Event-Time", "2026-11-21T17:40:15Z"),
    ///     ("ME-Event-Seq", "000001"),
    ///     ("ME-Harness", "claude-code"),
    ///     ("ME-Client-Version", "0.1.0"),
    /// ] {
    ///     headers.insert(name, value.parse().unwrap());
    /// }
    /// assert_eq!(Envelope::from_headers(&headers).unwrap().seq, 1);
    /// headers.remove("ME-Harness");
    /// assert_eq!(Envelope::from_headers(&headers), Err("ME-Harness"));
    /// ```
    pub fn from_headers(headers: &HeaderMap) -> Result<Self, &'static str> {
        let event_id = field(headers, "ME-Event-Id", is_event_id)?.to_owned();
        let time = {
            let raw = field(headers, "ME-Event-Time", |_| true)?;
            event_time(raw).ok_or("ME-Event-Time")?
        };
        let seq = {
            let raw = field(headers, "ME-Event-Seq", |s| {
                (1..=6).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
            })?;
            raw.parse().map_err(|_| "ME-Event-Seq")?
        };
        let harness = field(headers, "ME-Harness", |s| {
            (1..=32).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })?
        .to_owned();
        let client_version = field(headers, "ME-Client-Version", |s| {
            (1..=64).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
        })?
        .to_owned();
        Ok(Self {
            event_id,
            time,
            seq,
            harness,
            client_version,
        })
    }
}

/// An event ID as a log names it. The grammar admits `me_` and 32 base62 characters, a machine
/// token, so the ID goes through the machine-token rule first, on every path.
fn logged(event_id: &str) -> String {
    memory_engine::redact::scrub_provenance(event_id)
}

/// The one value of header `name` when it is present once, short enough, visible ASCII and
/// accepted by `grammar`.
pub(crate) fn field<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
    grammar: impl Fn(&str) -> bool,
) -> Result<&'a str, &'static str> {
    let mut values = headers.get_all(name).iter();
    match (values.next(), values.next()) {
        (Some(value), None) if value.len() <= MAX_HEADER_BYTES => {
            value.to_str().ok().filter(|s| grammar(s)).ok_or(name)
        }
        _ => Err(name),
    }
}

/// `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`.
fn is_event_id(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// `YYYY-MM-DDTHH:MM:SSZ`, UTC, whole seconds. The shape is checked before chrono reads it, so
/// nothing chrono tolerates beyond the grammar gets in.
pub(crate) fn event_time(s: &str) -> Option<DateTime<Utc>> {
    let b = s.as_bytes();
    let digits = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18];
    let shaped = b.len() == 20
        && [
            (4, b'-'),
            (7, b'-'),
            (10, b'T'),
            (13, b':'),
            (16, b':'),
            (19, b'Z'),
        ]
        .iter()
        .all(|&(at, c)| b[at] == c)
        && digits.iter().all(|&at| b[at].is_ascii_digit());
    if !shaped {
        return None;
    }
    let time = NaiveDateTime::parse_from_str(&s[..19], "%Y-%m-%dT%H:%M:%S").ok()?;
    // A leap second parses with its extra nanosecond; the grammar has none.
    (time.nanosecond() == 0).then(|| time.and_utc())
}

/// Why a body was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyError {
    /// More than the limit: 413.
    TooLarge,
    /// No byte for the stall time: 408.
    Stalled,
    /// The connection or the framing failed: 400.
    Broken,
}

/// Read `body` whole, at most `limit` bytes, giving up when no frame arrives for `stall`.
///
/// # Errors
///
/// [`BodyError::TooLarge`] as soon as the running total passes `limit` (a body of exactly
/// `limit` bytes is read), [`BodyError::Stalled`] when a frame takes longer than `stall`,
/// [`BodyError::Broken`] when the body fails.
///
/// # Examples
///
/// ```
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// use std::time::Duration;
/// use axum::body::Body;
/// use memory_engine_mcp::lifecycle::{BodyError, read_body};
///
/// let ok = read_body(Body::from("12345678"), 8, Duration::from_secs(1)).await;
/// assert_eq!(ok.unwrap(), b"12345678");
/// let big = read_body(Body::from("123456789"), 8, Duration::from_secs(1)).await;
/// assert_eq!(big, Err(BodyError::TooLarge));
/// # });
/// ```
pub async fn read_body<B>(body: B, limit: usize, stall: Duration) -> Result<Vec<u8>, BodyError>
where
    B: HttpBody<Data = Bytes>,
{
    let mut body = pin!(body);
    let mut bytes = Vec::new();
    loop {
        let next = tokio::time::timeout(stall, poll_fn(|cx| body.as_mut().poll_frame(cx)))
            .await
            .map_err(|_| BodyError::Stalled)?;
        match next {
            None => return Ok(bytes),
            Some(Err(_)) => return Err(BodyError::Broken),
            // A trailers frame carries no data.
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    if data.len() > limit - bytes.len() {
                        return Err(BodyError::TooLarge);
                    }
                    bytes.extend_from_slice(&data);
                }
            }
        }
    }
}

/// What the route answers: a status, and for a 204 what it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer {
    /// The status.
    pub status: StatusCode,
    /// `stored`, `duplicate` or `quarantined`: the `ME-Event-Outcome` of a 204.
    pub outcome: Option<&'static str>,
}

impl Answer {
    const fn status(status: StatusCode) -> Self {
        Self {
            status,
            outcome: None,
        }
    }

    const fn done(outcome: &'static str) -> Self {
        Self {
            status: StatusCode::NO_CONTENT,
            outcome: Some(outcome),
        }
    }
}

impl IntoResponse for Answer {
    fn into_response(self) -> Response {
        let mut response = self.status.into_response();
        if let Some(outcome) = self.outcome {
            response
                .headers_mut()
                .insert(OUTCOME.clone(), HeaderValue::from_static(outcome));
        }
        response
    }
}

/// The status of a failure while applying one event.
///
/// Only a store that cannot take the write now is a 5xx, because only then will the same request
/// work later. Anything else, an unexpected error and a panic included, is the event's own and
/// answers 422, so a retry never stops the client's queue for good.
///
/// # Examples
///
/// ```
/// use axum::http::StatusCode;
/// use memory_engine::error::{MemoryError, StorageError};
/// use memory_engine_mcp::lifecycle::status_of;
///
/// assert_eq!(
///     status_of(&MemoryError::Storage(StorageError::Unavailable("busy".into()))),
///     StatusCode::SERVICE_UNAVAILABLE
/// );
/// assert_eq!(status_of(&MemoryError::Internal("bug".into())), StatusCode::UNPROCESSABLE_ENTITY);
/// ```
#[must_use]
pub const fn status_of(error: &MemoryError) -> StatusCode {
    match error {
        MemoryError::Storage(StorageError::Unavailable(_)) => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    }
}

/// What an error is, for the log: never its message, which may quote the event.
pub(crate) const fn error_class(error: &MemoryError) -> &'static str {
    match error {
        MemoryError::Storage(StorageError::Unavailable(_)) => "storage unavailable",
        MemoryError::Storage(_) => "storage",
        MemoryError::Conflict(_) => "conflict",
        MemoryError::ReadOnly => "read only",
        MemoryError::Pool(_) => "pool",
        _ => "other",
    }
}

/// The route: the envelope, the body, the fence, then [`MemoryMcpServer::record_lifecycle`].
///
/// The body is read before the fence is taken, so a slow upload never holds a shutdown. The
/// engine call runs in a spawned task that holds the fence, so a client that hangs up never
/// cancels the steps after the commit.
pub(crate) async fn events(
    State(server): State<MemoryMcpServer>,
    Extension(machine): Extension<Machine>,
    request: Request,
) -> Response {
    let (parts, body) = request.into_parts();
    let envelope = match Envelope::from_headers(&parts.headers) {
        Ok(envelope) => envelope,
        Err(header) => {
            tracing::warn!(
                machine = machine.as_str(),
                header,
                "lifecycle envelope refused"
            );
            return Answer::status(StatusCode::BAD_REQUEST).into_response();
        }
    };
    let declared = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let event_id = logged(&envelope.event_id);
    let refuse = |status: StatusCode, why: &str| {
        tracing::warn!(
            machine = machine.as_str(),
            event_id,
            status = status.as_u16(),
            why,
            "lifecycle body refused"
        );
        Answer::status(status).into_response()
    };
    if declared.is_some_and(|n| n > BODY_LIMIT as u64) {
        return refuse(StatusCode::PAYLOAD_TOO_LARGE, "declared over the limit");
    }
    let body = match read_body(Body::new(body), BODY_LIMIT, BODY_STALL).await {
        Ok(body) => body,
        Err(BodyError::TooLarge) => return refuse(StatusCode::PAYLOAD_TOO_LARGE, "over the limit"),
        Err(BodyError::Stalled) => return refuse(StatusCode::REQUEST_TIMEOUT, "stalled"),
        Err(BodyError::Broken) => return refuse(StatusCode::BAD_REQUEST, "broken"),
    };
    let Ok(fence) = server.in_flight.clone().try_read_owned() else {
        tracing::warn!(
            machine = machine.as_str(),
            event_id,
            "lifecycle event refused: the server is stopping"
        );
        return Answer::status(StatusCode::SERVICE_UNAVAILABLE).into_response();
    };
    let headers = parts.headers;
    let applied = tokio::spawn(async move {
        let _fence = fence;
        server
            .record_lifecycle(&machine, &envelope, &headers, &body)
            .await
    })
    .await;
    applied
        .unwrap_or_else(|_| {
            tracing::error!("a lifecycle task panicked");
            Answer::status(StatusCode::UNPROCESSABLE_ENTITY)
        })
        .into_response()
}

/// How often the sweep looks for sessions that went quiet.
const SWEEP_EVERY: Duration = Duration::from_secs(60);

/// End idle lifecycle segments (ADR-0020 §5, rule 4): once at start, which finishes the sessions
/// that went quiet while the server was down, and then every minute, until the task is aborted.
///
/// Each tick holds `fence` for reading, so a stop that takes it for writing waits for a sweep in
/// progress and lets none start. A failed sweep is logged and retried at the next tick: nothing
/// is lost, because a segment stays open until some sweep ends it.
///
/// # Examples
///
/// ```no_run
/// use std::sync::Arc;
/// use memory_engine::MemoryEngine;
///
/// let runtime = tokio::runtime::Runtime::new()?;
/// runtime.block_on(async {
///     let engine = Arc::new(MemoryEngine::builder(4).build()?);
///     let fence = Arc::new(tokio::sync::RwLock::new(()));
///     let sweep = tokio::spawn(memory_engine_mcp::lifecycle::sweep(engine, fence));
///     sweep.abort();
///     Ok::<(), memory_engine::MemoryError>(())
/// })?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub async fn sweep(
    engine: std::sync::Arc<memory_engine::MemoryEngine>,
    fence: std::sync::Arc<tokio::sync::RwLock<()>>,
) {
    let mut ticks = tokio::time::interval(SWEEP_EVERY);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let _fence = fence.read().await;
        match engine.end_idle_segments(Utc::now()).await {
            Ok(0) => {}
            Ok(ended) => tracing::info!(ended, "ended idle lifecycle segments"),
            Err(error) => {
                tracing::warn!(
                    class = error_class(&error),
                    "the idle sweep failed; retrying at the next tick"
                );
            }
        }
    }
}

impl MemoryMcpServer {
    /// Everything after the envelope check, the body read and the fence (ADR-0020 §3): choose
    /// the adapter by `ME-Harness`, read the key, parse, map and apply the body, and answer.
    ///
    /// This is the seam the route calls and the fuzz target drives: total, every input answers
    /// 204 (with its outcome), 409, 422 or 503, and none panics.
    pub async fn record_lifecycle(
        &self,
        machine: &Machine,
        envelope: &Envelope,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Answer {
        let answer = self.apply_lifecycle(machine, envelope, headers, body).await;
        let (machine, event_id, status) = (
            machine.as_str(),
            logged(&envelope.event_id),
            answer.status.as_u16(),
        );
        if answer.status.is_success() {
            tracing::debug!(
                machine,
                event_id,
                status,
                outcome = answer.outcome,
                "lifecycle event"
            );
        } else {
            tracing::warn!(machine, event_id, status, "lifecycle event refused");
        }
        answer
    }

    async fn apply_lifecycle(
        &self,
        machine: &Machine,
        envelope: &Envelope,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Answer {
        let received_at = Utc::now();
        if envelope.harness != "claude-code" {
            return Answer::status(StatusCode::UNPROCESSABLE_ENTITY);
        }
        let facts = ProjectFacts::from_headers(headers);
        let key = facts.key(machine);
        let delivery = Delivery {
            machine: machine.as_str().to_owned(),
            event_id: envelope.event_id.clone(),
            time: envelope.time,
            seq: envelope.seq,
            harness: envelope.harness.clone(),
            client_version: envelope.client_version.clone(),
            body_sha256: Sha256::digest(body).into(),
            body_size: body.len() as u64,
            received_at,
            home: Home::own(key.clone(), machine).path(&self.user),
            provenance: facts.provenance(key.as_ref()),
        };
        let owned = body.to_vec();
        let Ok(read) = tokio::task::spawn_blocking(move || claude_code::read(&owned)).await else {
            return Answer::status(StatusCode::UNPROCESSABLE_ENTITY);
        };
        match self.engine.record_lifecycle_event(&delivery, read).await {
            Ok(Recorded::Stored { .. }) => Answer::done("stored"),
            Ok(Recorded::Duplicate) => Answer::done("duplicate"),
            Ok(Recorded::Quarantined) => Answer::done("quarantined"),
            Ok(Recorded::Collision) => Answer::status(StatusCode::CONFLICT),
            Ok(Recorded::Refused(_)) => Answer::status(StatusCode::UNPROCESSABLE_ENTITY),
            Err(error) => {
                tracing::warn!(
                    machine = machine.as_str(),
                    event_id = logged(&envelope.event_id),
                    class = error_class(&error),
                    "lifecycle event failed"
                );
                Answer::status(status_of(&error))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: [(&str, &str); 5] = [
        ("ME-Event-Id", "1795282815-000001-4242"),
        ("ME-Event-Time", "2026-11-21T17:40:15Z"),
        ("ME-Event-Seq", "000001"),
        ("ME-Harness", "claude-code"),
        ("ME-Client-Version", "0.1.0"),
    ];

    fn headers(overrides: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in GOOD {
            if !overrides.iter().any(|(n, _)| *n == name) {
                map.insert(name, value.parse().unwrap());
            }
        }
        for (name, value) in overrides {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        map
    }

    #[test]
    fn envelope_headers_follow_their_grammar() {
        let envelope = Envelope::from_headers(&headers(&[])).unwrap();
        assert_eq!(envelope.event_id, "1795282815-000001-4242");
        assert_eq!(envelope.seq, 1);
        assert_eq!(envelope.time.to_rfc3339(), "2026-11-21T17:40:15+00:00");

        let long = |n: usize, c: char| c.to_string().repeat(n);
        let cases: Vec<(&str, String, bool)> = vec![
            ("ME-Event-Id", "a".into(), true),
            ("ME-Event-Id", "9a._-".into(), true),
            ("ME-Event-Id", long(64, 'a'), true),
            ("ME-Event-Id", long(65, 'a'), false),
            ("ME-Event-Id", ".a".into(), false),
            ("ME-Event-Id", "-a".into(), false),
            ("ME-Event-Id", "a b".into(), false),
            ("ME-Event-Id", "a/b".into(), false),
            ("ME-Event-Id", String::new(), false),
            ("ME-Event-Time", "2026-11-21T17:40:15Z".into(), true),
            ("ME-Event-Time", "2024-02-29T00:00:00Z".into(), true),
            ("ME-Event-Time", "2026-11-21T17:40:15z".into(), false),
            ("ME-Event-Time", "2026-11-21 17:40:15Z".into(), false),
            ("ME-Event-Time", "2026-11-21T17:40:15+00:00".into(), false),
            ("ME-Event-Time", "2026-11-21T17:40:15.5Z".into(), false),
            ("ME-Event-Time", "2026-13-21T17:40:15Z".into(), false),
            ("ME-Event-Time", "2026-02-30T17:40:15Z".into(), false),
            ("ME-Event-Time", "2026-11-21T24:00:00Z".into(), false),
            ("ME-Event-Time", "2026-12-31T23:59:60Z".into(), false),
            ("ME-Event-Time", "2026-11-21T17:40:1\u{661}Z".into(), false),
            ("ME-Event-Seq", "0".into(), true),
            ("ME-Event-Seq", "000000".into(), true),
            ("ME-Event-Seq", "999999".into(), true),
            ("ME-Event-Seq", "1000000".into(), false),
            ("ME-Event-Seq", String::new(), false),
            ("ME-Event-Seq", "-1".into(), false),
            ("ME-Event-Seq", "+1".into(), false),
            ("ME-Event-Seq", "1a".into(), false),
            ("ME-Harness", "codex".into(), true),
            ("ME-Harness", long(32, 'a'), true),
            ("ME-Harness", long(33, 'a'), false),
            ("ME-Harness", "Codex".into(), false),
            ("ME-Harness", "a_b".into(), false),
            ("ME-Harness", String::new(), false),
            ("ME-Client-Version", "1.2.3-rc.1+build".into(), true),
            ("ME-Client-Version", long(64, '1'), true),
            ("ME-Client-Version", long(65, '1'), false),
            ("ME-Client-Version", "1 2".into(), false),
            ("ME-Client-Version", "1_2".into(), false),
            ("ME-Client-Version", long(4097, '1'), false),
        ];
        for (name, value, ok) in cases {
            let got = Envelope::from_headers(&headers(&[(name, &value)]));
            assert_eq!(got.is_ok(), ok, "{name}: {value:?}");
            if !ok {
                assert_eq!(got, Err(name), "{name}: {value:?}");
            }
        }
        for (name, _) in GOOD {
            let mut missing = headers(&[]);
            missing.remove(name);
            assert_eq!(
                Envelope::from_headers(&missing),
                Err(name),
                "missing {name}"
            );
            let mut repeated = headers(&[]);
            repeated.append(
                name,
                GOOD.iter()
                    .find(|(n, _)| *n == name)
                    .unwrap()
                    .1
                    .parse()
                    .unwrap(),
            );
            assert_eq!(
                Envelope::from_headers(&repeated),
                Err(name),
                "repeated {name}"
            );
        }
        let mut raw = headers(&[]);
        raw.insert(
            "ME-Harness",
            HeaderValue::from_bytes(b"claude\xffcode").unwrap(),
        );
        assert_eq!(Envelope::from_headers(&raw), Err("ME-Harness"));
    }

    /// `read_body` behind a real hyper connection, the limit 8 bytes and the stall 50 ms: what
    /// it answers to the request `head`, then `rest`, sent as a client would.
    async fn answer_to(head: &str, rest: &[&[u8]]) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let app = axum::Router::new().route(
            "/",
            axum::routing::post(|body: Body| async move {
                match read_body(body, 8, Duration::from_millis(50)).await {
                    Ok(_) => StatusCode::OK,
                    Err(BodyError::TooLarge) => StatusCode::PAYLOAD_TOO_LARGE,
                    Err(BodyError::Stalled) => StatusCode::REQUEST_TIMEOUT,
                    Err(BodyError::Broken) => StatusCode::BAD_REQUEST,
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, app).into_future());
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(format!("POST / HTTP/1.1\r\nHost: x\r\n{head}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        for chunk in rest {
            stream.write_all(chunk).await.unwrap();
        }
        let mut answer = [0; 12];
        stream.read_exact(&mut answer).await.unwrap();
        String::from_utf8(answer[9..12].to_vec())
            .unwrap()
            .parse()
            .unwrap()
    }

    #[tokio::test]
    async fn a_stalled_body_is_408_and_one_past_the_limit_413() {
        // No byte for 50 ms, before the first and after some.
        assert_eq!(answer_to("Content-Length: 5", &[]).await, 408);
        assert_eq!(answer_to("Content-Length: 5", &[b"ab"]).await, 408);
        // Exactly the limit passes; one past it does not, declared or streamed in chunks.
        assert_eq!(answer_to("Content-Length: 8", &[b"12345678"]).await, 200);
        assert_eq!(answer_to("Content-Length: 9", &[b"123456789"]).await, 413);
        let chunked = "Transfer-Encoding: chunked";
        let two_chunks = |a: &'static [u8], b: &'static [u8]| [a, b, b"0\r\n\r\n"];
        assert_eq!(
            answer_to(chunked, &two_chunks(b"4\r\n1234\r\n", b"4\r\n5678\r\n")).await,
            200
        );
        assert_eq!(
            answer_to(chunked, &two_chunks(b"4\r\n1234\r\n", b"5\r\n56789\r\n")).await,
            413
        );
        assert_eq!(BODY_LIMIT, 16_777_216);
    }

    #[tokio::test]
    async fn a_body_is_read_whole_up_to_the_limit() {
        let stall = Duration::from_secs(1);
        let ok = read_body(Body::from("12345678"), 8, stall).await;
        assert_eq!(ok.unwrap(), b"12345678");
        let empty = read_body(Body::empty(), 8, stall).await;
        assert_eq!(empty.unwrap(), b"");
        let big = read_body(Body::from("123456789"), 8, stall).await;
        assert_eq!(big, Err(BodyError::TooLarge));
    }

    #[test]
    fn only_an_unavailable_store_is_a_5xx() {
        let unavailable = MemoryError::Storage(StorageError::Unavailable("busy".into()));
        assert_eq!(status_of(&unavailable), StatusCode::SERVICE_UNAVAILABLE);
        for error in [
            MemoryError::Storage(StorageError::Backend("constraint".into())),
            MemoryError::Internal("bug".into()),
            MemoryError::ReadOnly,
            MemoryError::Pool("join".into()),
        ] {
            assert_eq!(
                status_of(&error),
                StatusCode::UNPROCESSABLE_ENTITY,
                "{error:?}"
            );
        }
    }
}
