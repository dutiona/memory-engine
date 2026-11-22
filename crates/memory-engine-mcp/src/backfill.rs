//! `POST /backfill/uploads` (#1026): one source file per request, imported as a dry run or as
//! the write of a reviewed dry run.
//!
//! The file comes from a machine that holds copies of its own history, and goes through the
//! engine's backfill import under the capture contract.
//!
//! The route checks in the order of `docs/reference/mcp-server.md` § "Backfill uploads": the
//! headers (400), the harness and kind (422), the denylist (503), the declared size (413), the
//! upload slot (503), the body (413, 408, 400), the fence (503), then the reader (422), and for a
//! write the collisions (409) and the preview (412). Nothing is written on any answer but a 200
//! to a write. Every answer but a 401 is JSON: the report, or a fixed phrase naming a header or
//! a reason, never a value or a byte of the body.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use axum::body::Body;
use axum::extract::{Extension, Request, State};
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, SecondsFormat, Utc};
use memory_engine::error::{MemoryError, StorageError};
use memory_engine::types::lifecycle::{BatchMode, ImportOutcome, Imported, Upload};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::adapters::{Mapped, claude_code};
use crate::auth::Machine;
use crate::home::Home;
use crate::lifecycle::{BODY_STALL, BodyError, error_class, event_time, field, read_body};
use crate::project::{KEY_RULES_VERSION, ProjectFacts, decode};
use crate::server::MemoryMcpServer;

/// The body limit, 256 MiB: a larger file is refused whole, never truncated.
pub const BODY_LIMIT: usize = memory_engine::BACKFILL_BODY_BYTES;

/// The answer to every upload while the server holds no denylist (decision 9 of the plan).
const NO_DENYLIST: &str =
    "no author denylist is loaded: set ME_REDACT_DENYLIST_FILE to a readable file and restart";

/// The headers of one upload (decision 3 of the plan).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadEnvelope {
    /// `ME-Harness`: selects the adapter.
    pub harness: String,
    /// `ME-Backfill-Kind`: selects the adapter's reader.
    pub kind: String,
    /// `ME-Backfill-Mode`, with a write's `ME-Backfill-Preview`.
    pub mode: BatchMode,
    /// `ME-Backfill-Source`, decoded: the file's path under the bundle root.
    pub source: String,
    /// `ME-Client-Version`, stored as sent.
    pub client_version: String,
    /// `ME-Backfill-Mtime`: the copy's modification time; required for a `memory-file`.
    pub mtime: Option<DateTime<Utc>>,
}

/// `[a-z0-9-]{1,32}`: a harness or a kind.
fn is_name(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A path under the bundle root: relative, without a `.` or `..` segment.
fn is_relative(path: &str) -> bool {
    !path.starts_with('/')
        && path
            .split('/')
            .all(|segment| !matches!(segment, "." | ".."))
}

fn is_preview(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

impl UploadEnvelope {
    /// Read an upload's headers, each at most once, within 4,096 bytes and its grammar.
    ///
    /// # Errors
    ///
    /// The name of the first header that is missing, repeated, too long or off its grammar, in
    /// the order `ME-Harness`, `ME-Backfill-Kind`, `ME-Backfill-Mode`, `ME-Backfill-Source`,
    /// `ME-Client-Version`, `ME-Backfill-Mtime` (required for a `memory-file`, checked whenever
    /// sent), `ME-Backfill-Preview` (required for a `write`).
    ///
    /// # Examples
    ///
    /// ```
    /// use axum::http::HeaderMap;
    /// use memory_engine::types::lifecycle::BatchMode;
    /// use memory_engine_mcp::backfill::UploadEnvelope;
    ///
    /// let mut headers = HeaderMap::new();
    /// for (name, value) in [
    ///     ("ME-Harness", "claude-code"),
    ///     ("ME-Backfill-Kind", "transcript"),
    ///     ("ME-Backfill-Mode", "dry-run"),
    ///     ("ME-Backfill-Source", "projects/-work-p/s1.jsonl"),
    ///     ("ME-Client-Version", "0.1.0"),
    /// ] {
    ///     headers.insert(name, value.parse().unwrap());
    /// }
    /// assert_eq!(UploadEnvelope::from_headers(&headers).unwrap().mode, BatchMode::DryRun);
    /// headers.insert("ME-Backfill-Mode", "write".parse().unwrap());
    /// assert_eq!(UploadEnvelope::from_headers(&headers), Err("ME-Backfill-Preview"));
    /// ```
    pub fn from_headers(headers: &HeaderMap) -> Result<Self, &'static str> {
        let harness = field(headers, "ME-Harness", is_name)?.to_owned();
        let kind = field(headers, "ME-Backfill-Kind", is_name)?.to_owned();
        let write = field(headers, "ME-Backfill-Mode", |m| {
            matches!(m, "dry-run" | "write")
        })? == "write";
        let source = field(headers, "ME-Backfill-Source", |_| true).and_then(|raw| {
            decode(raw.as_bytes())
                .ok()
                .filter(|s| is_relative(s))
                .ok_or("ME-Backfill-Source")
        })?;
        let client_version = field(headers, "ME-Client-Version", |s| {
            (1..=64).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
        })?
        .to_owned();
        let mtime = if kind == "memory-file" || headers.contains_key("ME-Backfill-Mtime") {
            let raw = field(headers, "ME-Backfill-Mtime", |_| true)?;
            Some(event_time(raw).ok_or("ME-Backfill-Mtime")?)
        } else {
            None
        };
        let mode = if write {
            let raw = field(headers, "ME-Backfill-Preview", is_preview)?;
            let mut preview = [0; 32];
            for (byte, pair) in preview.iter_mut().zip(raw.as_bytes().chunks(2)) {
                let pair = std::str::from_utf8(pair).map_err(|_| "ME-Backfill-Preview")?;
                *byte = u8::from_str_radix(pair, 16).map_err(|_| "ME-Backfill-Preview")?;
            }
            BatchMode::Write { preview }
        } else {
            BatchMode::DryRun
        };
        Ok(Self {
            harness,
            kind,
            mode,
            source,
            client_version,
            mtime,
        })
    }

    const fn mode_name(&self) -> &'static str {
        match self.mode {
            BatchMode::DryRun => "dry-run",
            BatchMode::Write { .. } => "write",
        }
    }
}

/// Whether this server has a reader for `(harness, kind)`: the `match` of ADR-0020 §1.
fn known(harness: &str, kind: &str) -> bool {
    matches!(
        (harness, kind),
        ("claude-code", "transcript" | "memory-file")
    )
}

/// A JSON answer.
fn answer(status: StatusCode, body: &Value) -> Response {
    let mut response = (status, serde_json::to_vec(body).unwrap_or_default()).into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

fn error(reason: &str) -> Value {
    json!({ "error": reason })
}

/// The route: the headers, the slot, the body and the fence, then
/// [`MemoryMcpServer::import_upload`] in a spawned task that holds the fence and the slot, so a
/// client that hangs up never cancels the steps after the commit.
pub(crate) async fn uploads(
    State(server): State<MemoryMcpServer>,
    Extension(machine): Extension<Machine>,
    request: Request,
) -> Response {
    let (parts, body) = request.into_parts();
    let refuse = |status: StatusCode, why: &str| {
        tracing::warn!(
            machine = machine.as_str(),
            status = status.as_u16(),
            why,
            "backfill upload refused"
        );
        answer(status, &error(why))
    };
    let envelope = match UploadEnvelope::from_headers(&parts.headers) {
        Ok(envelope) => envelope,
        Err(header) => return refuse(StatusCode::BAD_REQUEST, header),
    };
    if !known(&envelope.harness, &envelope.kind) {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "an unknown harness or kind",
        );
    }
    if server.denylist.is_none() {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, NO_DENYLIST);
    }
    let declared = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > BODY_LIMIT as u64) {
        return refuse(StatusCode::PAYLOAD_TOO_LARGE, "the body is over 256 MiB");
    }
    let Ok(slot) = std::sync::Arc::clone(&server.uploads).try_acquire_owned() else {
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "another upload is in flight",
        );
    };
    let body = match read_body(Body::new(body), BODY_LIMIT, BODY_STALL).await {
        Ok(body) => body,
        Err(BodyError::TooLarge) => {
            return refuse(StatusCode::PAYLOAD_TOO_LARGE, "the body is over 256 MiB");
        }
        Err(BodyError::Stalled) => return refuse(StatusCode::REQUEST_TIMEOUT, "the body stalled"),
        Err(BodyError::Broken) => return refuse(StatusCode::BAD_REQUEST, "the body is broken"),
    };
    let Ok(fence) = server.in_flight.clone().try_read_owned() else {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "the server is stopping");
    };
    let headers = parts.headers;
    let imported = tokio::spawn(async move {
        let (_fence, _slot) = (fence, slot);
        server
            .import_upload(&machine, &envelope, &headers, &body)
            .await
    })
    .await;
    let (status, body) = imported.unwrap_or_else(|_| {
        tracing::error!("a backfill upload task panicked");
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            error("the upload could not be imported"),
        )
    });
    answer(status, &body)
}

/// What an import failure answers: only a store that cannot take the write now is a 5xx.
fn failure(error: &MemoryError) -> (StatusCode, Value) {
    match error {
        MemoryError::Storage(StorageError::Unavailable(_)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            self::error("the store cannot take the write now"),
        ),
        MemoryError::Conflict(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            self::error("the upload's source does not settle under redaction"),
        ),
        _ => (
            StatusCode::UNPROCESSABLE_ENTITY,
            self::error("the upload could not be imported"),
        ),
    }
}

impl MemoryMcpServer {
    /// Everything after the headers, the slot, the body read and the fence: the project facts
    /// and their Home, the reader the harness and kind select, the engine's import, and the
    /// answer with its report.
    ///
    /// This is the seam the route calls: total, every input answers 200, 409, 412, 422 or 503
    /// with a JSON body, and none panics.
    pub async fn import_upload(
        &self,
        machine: &Machine,
        envelope: &UploadEnvelope,
        headers: &HeaderMap,
        body: &[u8],
    ) -> (StatusCode, Value) {
        let (status, report, class) = self.apply_upload(machine, envelope, headers, body).await;
        let (machine, kind, mode, status_code) = (
            machine.as_str(),
            envelope.kind.as_str(),
            envelope.mode_name(),
            status.as_u16(),
        );
        if status.is_success() {
            tracing::info!(machine, kind, mode, status = status_code, "backfill upload");
        } else {
            tracing::warn!(
                machine,
                kind,
                mode,
                status = status_code,
                class,
                "backfill upload refused"
            );
        }
        (status, report)
    }

    async fn apply_upload(
        &self,
        machine: &Machine,
        envelope: &UploadEnvelope,
        headers: &HeaderMap,
        body: &[u8],
    ) -> (StatusCode, Value, &'static str) {
        let received_at = Utc::now();
        let Some(denylist) = self.denylist.clone() else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                error(NO_DENYLIST),
                "denylist",
            );
        };
        let facts = ProjectFacts::from_headers(headers);
        let key = facts.key(machine);
        let home = Home::own(key.clone(), machine).path(&self.user);
        let provenance = facts.provenance(key.as_ref());
        let (harness, kind) = (envelope.harness.clone(), envelope.kind.clone());
        let name = envelope
            .source
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        let mtime = envelope.mtime.unwrap_or(received_at);
        let (owned, reader_home) = (body.to_vec(), home.clone());
        let read = tokio::task::spawn_blocking(move || match (harness.as_str(), kind.as_str()) {
            ("claude-code", "transcript") => claude_code::transcript::read(&owned),
            ("claude-code", "memory-file") => {
                claude_code::memory::read(&name, &reader_home, &owned, mtime)
            }
            _ => Err("an unknown harness or kind"),
        })
        .await;
        let mut census = match read {
            Ok(Ok(mapped)) => mapped,
            Ok(Err(reason)) => return (StatusCode::UNPROCESSABLE_ENTITY, error(reason), "read"),
            Err(_) => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    error("the upload could not be read"),
                    "read",
                );
            }
        };
        let items = std::mem::take(&mut census.items);
        let upload = Upload {
            machine: machine.as_str().to_owned(),
            harness: envelope.harness.clone(),
            client_version: envelope.client_version.clone(),
            kind: envelope.kind.clone(),
            source: envelope.source.clone(),
            received_at,
            home,
            provenance,
            body_sha256: Sha256::digest(body).into(),
            census: json!({
                "lines": census.lines,
                "malformed": census.malformed,
                "skipped": census.skipped,
                "pending": census.pending,
                "incomplete_turns": census.incomplete_turns,
            }),
        };
        let imported = match self
            .engine
            .import_backfill(&upload, items, &denylist, envelope.mode)
            .await
        {
            Ok(imported) => imported,
            Err(e) => {
                let (status, body) = failure(&e);
                return (status, body, error_class(&e));
            }
        };
        let write = matches!(envelope.mode, BatchMode::Write { .. });
        let collided = imported
            .items
            .iter()
            .any(|(o, _)| *o == ImportOutcome::Collision);
        if write && !imported.committed && !collided {
            return (
                StatusCode::PRECONDITION_FAILED,
                error("the upload's effects changed since its dry run: review a new dry run"),
                "preview",
            );
        }
        let report = Report {
            envelope,
            upload: &upload,
            body,
            denylist: denylist.len(),
            facts: &facts,
            key: key.as_ref().map(crate::project::ProjectKey::as_str),
            census: &census,
            imported: &imported,
        }
        .render();
        if write && collided {
            (StatusCode::CONFLICT, report, "collision")
        } else {
            (StatusCode::OK, report, "")
        }
    }
}

fn time(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The upload's report (decision 11 of the plan), one schema for both modes and for a 409.
/// Every string is a count, a digest, a fixed phrase, an identifier that passed the
/// machine-token rule, or text exactly as the store would hold it.
struct Report<'a> {
    envelope: &'a UploadEnvelope,
    upload: &'a Upload,
    body: &'a [u8],
    denylist: usize,
    facts: &'a ProjectFacts,
    key: Option<&'a str>,
    census: &'a Mapped,
    imported: &'a Imported,
}

/// A session's line in the report.
#[derive(Default)]
struct Session {
    first: Option<DateTime<Utc>>,
    last: Option<DateTime<Utc>>,
    events: usize,
}

/// The report's counts and lists, over the import's items. A refused item is counted by its
/// reason alone: its identifiers did not pass the engine's checks, so none is shown.
#[derive(Default)]
struct Tally<'a> {
    outcomes: BTreeMap<&'static str, usize>,
    refused: BTreeMap<&'static str, usize>,
    by_kind: BTreeMap<&'static str, usize>,
    by_actor: BTreeMap<&'static str, usize>,
    tools: BTreeMap<&'a str, BTreeMap<&'a str, usize>>,
    stubs: Vec<Value>,
    sessions: BTreeMap<&'a str, Session>,
    placeholders: BTreeMap<&'a str, u64>,
    collisions: Vec<Value>,
}

impl<'a> Tally<'a> {
    fn of(imported: &'a Imported) -> Self {
        let mut tally = Self::default();
        for (outcome, item) in &imported.items {
            let name = match outcome {
                ImportOutcome::Stored => "stored",
                ImportOutcome::Duplicate => "duplicate",
                ImportOutcome::Collision => "collision",
                ImportOutcome::LiveOverlap => "live_overlap",
                ImportOutcome::Refused(reason) => {
                    *tally.refused.entry(reason).or_default() += 1;
                    continue;
                }
            };
            *tally.outcomes.entry(name).or_default() += 1;
            if let Some(id) = item.session_id.as_deref() {
                let session = tally.sessions.entry(id).or_default();
                session.first = Some(session.first.map_or(item.time, |t| t.min(item.time)));
                session.last = Some(session.last.map_or(item.time, |t| t.max(item.time)));
                session.events += usize::from(*outcome == ImportOutcome::Stored);
            }
            if *outcome == ImportOutcome::Collision {
                let collision = json!({ "event_id": item.event_id, "line": item.line });
                tally.collisions.push(collision);
            }
            if *outcome != ImportOutcome::Stored {
                continue;
            }
            *tally.by_kind.entry(item.kind).or_default() += 1;
            *tally
                .by_actor
                .entry(item.actor.unwrap_or("none"))
                .or_default() += 1;
            if let (Some(tool), Some(result)) = (item.tool.as_deref(), item.outcome.as_deref()) {
                *tally
                    .tools
                    .entry(tool)
                    .or_default()
                    .entry(result)
                    .or_default() += 1;
            }
            if let Some(path) = &item.stub {
                let stub = json!({ "tool": item.tool, "path": path, "time": time(item.time) });
                tally.stubs.push(stub);
            }
            for (rule, n) in &item.placeholders {
                *tally.placeholders.entry(rule).or_default() += n;
            }
        }
        tally
    }
}

impl Report<'_> {
    fn render(&self) -> Value {
        let imported = self.imported;
        let Tally {
            outcomes,
            refused,
            by_kind,
            by_actor,
            tools,
            stubs,
            sessions,
            placeholders,
            collisions,
        } = Tally::of(imported);
        let sessions: Vec<Value> = sessions
            .into_iter()
            .map(|(id, s)| {
                json!({
                    "id": id,
                    "first": s.first.map(time),
                    "last": s.last.map(time),
                    "events": s.events,
                    "live_since": imported.live_since.get(id).map(|k| time(k.time)),
                })
            })
            .collect();
        let count = |name: &str| outcomes.get(name).copied().unwrap_or_default();
        let made = |outcome: &str| {
            imported
                .facts
                .iter()
                .filter(|f| f.outcome == outcome)
                .count()
        };
        let census = self.census;
        json!({
            "mode": self.envelope.mode_name(),
            "committed": imported.committed,
            "preview": hex(&imported.preview),
            "machine": self.upload.machine,
            "harness": self.upload.harness,
            "kind": self.upload.kind,
            "source": imported.source,
            "client_version": self.upload.client_version,
            "body": { "bytes": self.body.len(), "sha256": hex(&self.upload.body_sha256) },
            "denylist_literals": self.denylist,
            "project": {
                "home": self.upload.home,
                "key": self.key,
                "rules": KEY_RULES_VERSION,
                "dropped": self.facts.dropped,
            },
            "census": {
                "lines": census.lines,
                "malformed": census.malformed,
                "skipped": census.skipped,
                "incomplete_turn": census.incomplete_turns,
            },
            "sessions": sessions,
            "events": {
                "stored": count("stored"),
                "duplicate": count("duplicate"),
                "collision": count("collision"),
                "live_overlap": count("live_overlap"),
                "pending": census.pending,
                "refused": refused,
                "by_kind": by_kind,
                "by_actor": by_actor,
            },
            "tool_events": tools,
            "stubs": stubs,
            "redaction": {
                "found": imported.found,
                "stubbed": imported.stubbed,
                "placeholders": placeholders,
            },
            "facts": {
                "created": made("created"),
                "reinforced": made("reinforced"),
                "blocked": made("blocked"),
                "items": imported.facts.iter().map(|f| json!({
                    "event_id": f.event_id,
                    "capture": f.capture,
                    "fact_type": f.fact_type.to_string(),
                    "home": f.home,
                    "t_created": time(f.t_created),
                    "outcome": f.outcome,
                    "hint": f.hint,
                    "preview": f.preview,
                })).collect::<Vec<_>>(),
            },
            "collisions": collisions,
        })
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderName;

    use super::*;

    const GOOD: [(&str, &str); 5] = [
        ("ME-Harness", "claude-code"),
        ("ME-Backfill-Kind", "transcript"),
        ("ME-Backfill-Mode", "dry-run"),
        ("ME-Backfill-Source", "projects/-work-p/s1.jsonl"),
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
    fn upload_headers_follow_their_grammar() {
        let envelope = UploadEnvelope::from_headers(&headers(&[])).unwrap();
        assert_eq!(envelope.source, "projects/-work-p/s1.jsonl");
        assert_eq!((envelope.mode, envelope.mtime), (BatchMode::DryRun, None));

        let preview = "0123456789abcdef".repeat(4);
        let write = UploadEnvelope::from_headers(&headers(&[
            ("ME-Backfill-Mode", "write"),
            ("ME-Backfill-Preview", &preview),
        ]))
        .unwrap();
        let mut want = [0; 32];
        for (i, byte) in want.iter_mut().enumerate() {
            *byte = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef][i % 8];
        }
        assert_eq!(write.mode, BatchMode::Write { preview: want });
        let memory = UploadEnvelope::from_headers(&headers(&[
            ("ME-Backfill-Kind", "memory-file"),
            ("ME-Backfill-Mtime", "2026-03-20T08:00:00Z"),
        ]))
        .unwrap();
        assert_eq!(
            memory.mtime.map(|t| t.to_rfc3339()),
            Some("2026-03-20T08:00:00+00:00".to_owned())
        );
        assert_eq!(
            UploadEnvelope::from_headers(&headers(&[("ME-Backfill-Source", "a%20b/c%C3%A9.md")]))
                .unwrap()
                .source,
            "a b/cé.md",
            "decoded like a project fact"
        );

        let long = "a".repeat(4097);
        let cases: Vec<(&str, &str)> = vec![
            ("ME-Harness", "Claude"),
            ("ME-Harness", ""),
            ("ME-Backfill-Kind", "a_b"),
            ("ME-Backfill-Kind", "abcdefghijklmnopqrstuvwxyz0123456"),
            ("ME-Backfill-Mode", "Write"),
            ("ME-Backfill-Mode", "dry_run"),
            ("ME-Backfill-Source", "/etc/passwd"),
            ("ME-Backfill-Source", ".."),
            ("ME-Backfill-Source", "a/./b"),
            ("ME-Backfill-Source", "a/b/.."),
            ("ME-Backfill-Source", "a%00b"),
            ("ME-Backfill-Source", "a%2"),
            ("ME-Backfill-Source", &long),
            ("ME-Client-Version", "1_0"),
            ("ME-Backfill-Mtime", "2026-03-20T08:00:00+00:00"),
        ];
        for (name, value) in cases {
            assert_eq!(
                UploadEnvelope::from_headers(&headers(&[(name, value)])),
                Err(name),
                "{name}: {value:?}"
            );
        }
        for preview in ["", "0", &"0".repeat(63), &"0".repeat(65), &"F".repeat(64)] {
            assert_eq!(
                UploadEnvelope::from_headers(&headers(&[
                    ("ME-Backfill-Mode", "write"),
                    ("ME-Backfill-Preview", preview),
                ])),
                Err("ME-Backfill-Preview"),
                "{preview:?}"
            );
        }
        assert_eq!(
            UploadEnvelope::from_headers(&headers(&[("ME-Backfill-Kind", "memory-file")])),
            Err("ME-Backfill-Mtime"),
            "a memory file needs its mtime"
        );
        for (name, _) in GOOD {
            let mut missing = headers(&[]);
            missing.remove(name);
            assert_eq!(
                UploadEnvelope::from_headers(&missing),
                Err(name),
                "missing {name}"
            );
            let mut repeated = headers(&[]);
            repeated.append(name, "x".parse().unwrap());
            assert_eq!(
                UploadEnvelope::from_headers(&repeated),
                Err(name),
                "repeated {name}"
            );
        }
    }

    #[test]
    fn only_a_known_harness_and_kind_has_a_reader() {
        assert!(known("claude-code", "transcript"));
        assert!(known("claude-code", "memory-file"));
        assert!(!known("claude-code", "claude-mem"));
        assert!(!known("codex", "transcript"));
    }
}
