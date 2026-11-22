//! The backfill import (#1026): an upload's neutral items, applied under the capture contract as
//! if captured live, in one store transaction that a dry run rolls back.
//!
//! The core knows no harness field: a reader in the server hands it [`ImportItem`]s. Events go
//! through the lifecycle core's own `prepare`, so live and imported captures share one contract;
//! a memory becomes one fact recorded by one event.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::{ConflictError, MemoryError, Result};
use crate::limits::{check_json_size, check_str_size};
use crate::redact;
use crate::types::lifecycle::{
    Actor, Applied, BatchMode, Delivery, DeliveryWrite, FactPreview, ImportEvent, ImportItem,
    ImportMemory, ImportOutcome, Imported, ItemSummary, Keyed, Kind, MemoryWrite, OrderKey, Upload,
};
use crate::types::{EventType, NewEvent, NewFact};

use super::lifecycle::{
    Capture, Origin, Plan, delivery_off_invariants, holds_a_token, prepare, record_off_invariants,
};
use super::{MemoryEngine, spawn_join_err};

/// An event or memory label: never counted as live.
const BACKFILL: &str = "backfill";

/// Why a text cannot be stored: redaction did not settle on it.
const UNSETTLED: &str = "a text does not settle under redaction";

/// One item, prepared: what the store writes and what the report says of it.
struct Prepared {
    key: Option<OrderKey>,
    summary: ItemSummary,
    /// What the store writes, or why the engine refused the item, which the store never sees.
    write: std::result::Result<Keyed, &'static str>,
    /// The memory it makes, before the store says whether it was created.
    fact: Option<FactPreview>,
    found: u64,
    stubbed: u64,
}

impl Prepared {
    const fn refused(key: Option<OrderKey>, summary: ItemSummary, reason: &'static str) -> Self {
        Self {
            key,
            summary,
            write: Err(reason),
            fact: None,
            found: 0,
            stubbed: 0,
        }
    }
}

impl MemoryEngine {
    /// Import one backfill upload (#1026): its `items`, read from one source file by a harness
    /// reader, applied in one store transaction under the capture contract, as if captured live.
    ///
    /// Every item is redacted first, outside any lock, with every rule and the author
    /// `denylist` (never on an identifier); a record off the lifecycle core's invariants, or a
    /// text that does not settle, is [`ImportOutcome::Refused`] and skipped. The rest, in
    /// order-key order, goes to the store in one transaction: each `(machine, event ID)` key
    /// is read first, so a re-upload is all [`ImportOutcome::Duplicate`]; a capture at or after
    /// its session's first live event is [`ImportOutcome::LiveOverlap`]; a memory reinforces
    /// an active twin in its home or is queued for its vector, and a forget's tombstone blocks
    /// it. Imported events are labelled `backfill`, and their memories dated by their events.
    ///
    /// The preview is a digest of every effect the upload would write and of the store's
    /// outcomes, never of the receipt time or a row id. A [`BatchMode::DryRun`] rolls back and
    /// returns it; a [`BatchMode::Write`] commits only when no item collided and its preview is
    /// that one, the reviewed dry run's. Nothing is embedded: every memory is queued for the
    /// drain, so an import works with the embedder down. The scope cache, the drain's wake and
    /// the redaction totals change only after a commit.
    ///
    /// # Errors
    ///
    /// `MemoryError::ReadOnly` on a read-only engine; `Conflict(Redaction)` when the upload's
    /// source does not settle; `Storage(Unavailable)` when the store cannot take the write now
    /// and the same call can work later; any other storage failure. Nothing is written on any
    /// error.
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// use chrono::{Duration, Utc};
    /// use memory_engine::types::lifecycle::{
    ///     Actor, BatchMode, Content, ImportEvent, ImportItem, ImportOutcome, NeutralRecord,
    ///     StopOutcome, Upload,
    /// };
    ///
    /// let engine = memory_engine::MemoryEngine::builder(4).build()?;
    /// let upload = Upload {
    ///     machine: "laptop".into(),
    ///     harness: "claude-code".into(),
    ///     client_version: "0.1.0".into(),
    ///     kind: "transcript".into(),
    ///     source: "projects/demo/s1.jsonl".into(),
    ///     received_at: Utc::now(),
    ///     home: "user:alice/project:demo".into(),
    ///     provenance: serde_json::json!({ "facts": {}, "dropped": {}, "key": null, "rules": 1 }),
    ///     body_sha256: [9; 32],
    ///     census: serde_json::json!({ "lines": 1 }),
    /// };
    /// let stop = ImportItem::Event(ImportEvent {
    ///     event_id: "bf.4f6c.s".into(),
    ///     time: Utc::now() - Duration::days(30),
    ///     seq: 0,
    ///     body_sha256: [1; 32],
    ///     body_size: 120,
    ///     line: 7,
    ///     record: NeutralRecord {
    ///         session_id: "s1".into(),
    ///         actor: Actor::Main,
    ///         cwd: None,
    ///         harness_version: Some("2.1.296".into()),
    ///         content: Content::Stop {
    ///             outcome: StopOutcome::Completed,
    ///             message: Some("Shipped the flag.".into()),
    ///         },
    ///     },
    /// });
    ///
    /// let dry = engine.import_backfill(&upload, vec![stop.clone()], &[], BatchMode::DryRun).await?;
    /// assert!(!dry.committed);
    /// assert_eq!(dry.items[0].0, ImportOutcome::Stored);
    /// assert_eq!(dry.facts[0].outcome, "created");
    /// assert!(engine.list_active_facts(None).await?.is_empty(), "a dry run writes nothing");
    ///
    /// let write = BatchMode::Write { preview: dry.preview };
    /// assert!(engine.import_backfill(&upload, vec![stop], &[], write).await?.committed);
    /// # Ok::<(), memory_engine::MemoryError>(())
    /// # }).unwrap();
    /// ```
    pub async fn import_backfill(
        &self,
        upload: &Upload,
        items: Vec<ImportItem>,
        denylist: &[String],
        mode: BatchMode,
    ) -> Result<Imported> {
        self.ensure_open()?;
        self.ensure_writable()?;
        let (machine, received_at) = (upload.machine.clone(), upload.received_at);
        let (upload, denylist) = (upload.clone(), denylist.to_vec());
        let (source, context, prepared) =
            tokio::task::spawn_blocking(move || prepare_upload(&upload, items, &denylist))
                .await
                .map_err(spawn_join_err)??;
        let keyed: Vec<Keyed> = prepared
            .iter()
            .filter_map(|p| p.write.as_ref().ok().cloned())
            .collect();
        let batch = self
            .storage
            .record_deliveries(&machine, received_at, keyed, context, mode)
            .await?;

        let mut applied = batch.applied.into_iter();
        let (mut items, mut facts) = (Vec::new(), Vec::new());
        let (mut found, mut stubbed, mut queued) = (0, 0, false);
        let mut chains = Vec::new();
        for p in prepared {
            let outcome = match p.write {
                Err(reason) => ImportOutcome::Refused(reason),
                Ok(_) => match applied.next() {
                    Some(Applied::Stored {
                        fact,
                        reinforced,
                        chain,
                        ..
                    }) => {
                        found += p.found;
                        stubbed += p.stubbed;
                        queued |= fact.is_some() && !reinforced;
                        chains.push(chain);
                        if let Some(mut made) = p.fact {
                            made.outcome = match (fact, reinforced) {
                                (None, _) => "blocked",
                                (Some(_), true) => "reinforced",
                                (Some(_), false) => "created",
                            };
                            facts.push(made);
                        }
                        ImportOutcome::Stored
                    }
                    Some(Applied::Duplicate) => ImportOutcome::Duplicate,
                    Some(Applied::Collision) => ImportOutcome::Collision,
                    Some(Applied::LiveOverlap) => ImportOutcome::LiveOverlap,
                    other => {
                        return Err(MemoryError::Internal(format!(
                            "the store answered an imported item with {other:?}"
                        )));
                    }
                },
            };
            items.push((outcome, p.summary));
        }
        if batch.committed {
            for chain in chains {
                me_index::insert_scope_chain(&self.scope_tree, chain);
            }
            if queued {
                self.embed_work.notify_one();
            }
            self.tally(found, stubbed).await;
        }
        Ok(Imported {
            committed: batch.committed,
            preview: batch.preview,
            source,
            items,
            facts,
            live_since: batch.live_since,
            found,
            stubbed,
        })
    }
}

/// Redact and bound every item, sort them by order key, and digest their effects: the stored
/// source, the context the preview starts from, and the items. Redaction is blocking work.
fn prepare_upload(
    upload: &Upload,
    items: Vec<ImportItem>,
    denylist: &[String],
) -> Result<(String, [u8; 32], Vec<Prepared>)> {
    let (source, source_found) = redact::scrub_with(&upload.source, denylist)?;
    let mut prepared = items
        .into_iter()
        .map(|item| match item {
            ImportItem::Event(event) => prepare_event(upload, event, denylist),
            ImportItem::Memory(memory) => {
                prepare_memory(upload, &source, source_found, memory, denylist)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    prepared.sort_by(|a, b| a.key.cmp(&b.key));
    let context = context_digest(upload, &source, denylist, &prepared)?;
    Ok((source, context, prepared))
}

fn prepare_event(upload: &Upload, event: ImportEvent, denylist: &[String]) -> Result<Prepared> {
    let ImportEvent {
        event_id,
        time,
        seq,
        body_sha256,
        body_size,
        line,
        record,
    } = event;
    let kind = record.content.kind();
    let key = Some(OrderKey {
        time,
        seq,
        event_id: event_id.clone(),
    });
    let mut summary = ItemSummary {
        event_id: event_id.clone(),
        line: Some(line),
        kind: kind.as_str(),
        actor: Some(match record.actor {
            Actor::Main => "main",
            Actor::Subagent { .. } => "subagent",
        }),
        session_id: Some(record.session_id.clone()),
        time,
        tool: None,
        outcome: None,
        stub: None,
        placeholders: BTreeMap::new(),
    };
    let delivery = Delivery {
        machine: upload.machine.clone(),
        event_id,
        time,
        seq,
        harness: upload.harness.clone(),
        client_version: upload.client_version.clone(),
        body_sha256,
        body_size,
        received_at: upload.received_at,
        home: upload.home.clone(),
        provenance: upload.provenance.clone(),
    };
    if let Some(reason) =
        delivery_off_invariants(&delivery).or_else(|| record_off_invariants(&record))
    {
        return Ok(Prepared::refused(key, summary, reason));
    }
    let capture = Capture {
        denylist,
        origin: Origin::Backfill,
    };
    let (write, found, stubbed) = match prepare(&delivery, record, &capture) {
        Ok(Plan::Write {
            write: DeliveryWrite::Capture(write),
            found,
            stubbed,
        }) => (write, found, stubbed),
        Ok(Plan::Refuse(reason)) => return Ok(Prepared::refused(key, summary, reason)),
        Ok(Plan::Write { .. }) => {
            return Err(MemoryError::Internal(
                "a record was prepared as another write than a capture".into(),
            ));
        }
        Err(MemoryError::Conflict(ConflictError::Redaction(_))) => {
            return Ok(Prepared::refused(key, summary, UNSETTLED));
        }
        Err(e) => return Err(e),
    };
    summarize(&mut summary, &write.event.payload, stubbed > 0);
    let capture = if kind == Kind::Stop {
        "final_message"
    } else {
        "compaction_summary"
    };
    let fact = write
        .fact
        .as_ref()
        .map(|fact| preview_of(&delivery.event_id, capture, upload, fact, None));
    Ok(Prepared {
        key,
        summary,
        write: Ok(Keyed {
            event_id: delivery.event_id,
            body_sha256,
            write: DeliveryWrite::Capture(write),
        }),
        fact,
        found,
        stubbed,
    })
}

/// What the report says of a captured event, read from its redacted payload.
fn summarize(summary: &mut ItemSummary, payload: &Value, stub: bool) {
    let text = |member: &str| {
        payload
            .get(member)
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    if summary.kind == Kind::ToolUse.as_str() {
        summary.tool = text("tool");
        if stub {
            summary.stub = payload["input"]["sensitive_path"]
                .as_str()
                .map(str::to_owned);
        }
    }
    summary.outcome = text("outcome");
    count_placeholders(payload, &mut summary.placeholders);
}

fn prepare_memory(
    upload: &Upload,
    source: &str,
    source_found: u64,
    memory: ImportMemory,
    denylist: &[String],
) -> Result<Prepared> {
    let ImportMemory {
        event_id,
        body_sha256,
        item,
        ..
    } = memory;
    let mut summary = ItemSummary {
        event_id: event_id.clone(),
        line: None,
        kind: "memory",
        actor: None,
        session_id: None,
        time: item.time,
        tool: None,
        outcome: None,
        stub: None,
        placeholders: BTreeMap::new(),
    };
    if holds_a_token(&event_id) {
        return Ok(Prepared::refused(
            None,
            summary,
            "the event ID holds a machine token",
        ));
    }
    let mut metadata = item.metadata;
    let scrubbed = redact::scrub_with(&item.text, denylist)
        .and_then(|(text, found)| Ok((text, found + redact::scrub_json(&mut metadata, denylist)?)));
    let (text, found) = match scrubbed {
        Ok(scrubbed) => scrubbed,
        Err(MemoryError::Conflict(ConflictError::Redaction(_))) => {
            return Ok(Prepared::refused(None, summary, UNSETTLED));
        }
        Err(e) => return Err(e),
    };
    if check_str_size(&text, "fact content").is_err()
        || check_json_size(&metadata, "fact metadata").is_err()
    {
        return Ok(Prepared::refused(
            None,
            summary,
            "the memory is over the write limit",
        ));
    }
    if let Value::Object(members) = &mut metadata {
        members.insert("machine".to_owned(), json!(upload.machine));
    }
    let mut payload = json!({
        "machine": upload.machine,
        "kind": upload.kind,
        "source": source,
        "harness": upload.harness,
        "client_version": upload.client_version,
        redact::PROVENANCE_KEY: upload.provenance,
    });
    let payload_found = redact::scrub_capture_payload(&mut payload, denylist)?;
    let hint = (metadata.get("memory_type").and_then(Value::as_str) == Some("user"))
        .then_some("set_level");
    let fact = NewFact::builder(text, Vec::new(), item.fact_type)
        .base_importance(item.importance)
        .t_created(item.time)
        .t_valid(item.time.min(upload.received_at))
        .metadata(metadata)
        .build();
    count_placeholders(&payload, &mut summary.placeholders);
    count_placeholders(
        &json!([fact.content, fact.metadata]),
        &mut summary.placeholders,
    );
    let preview = preview_of(&event_id, "memory", upload, &fact, hint);
    let event = NewEvent {
        timestamp: item.time,
        event_type: EventType::SystemEvent,
        payload,
        source: BACKFILL.to_owned(),
        session_id: None,
        scope_id: 1,
        origin_node_id: BACKFILL.to_owned(),
        sequence_id: 0,
        created_at: Some(upload.received_at),
    };
    Ok(Prepared {
        key: None,
        summary,
        write: Ok(Keyed {
            event_id,
            body_sha256,
            write: DeliveryWrite::Memory(Box::new(MemoryWrite {
                home: upload.home.clone(),
                event,
                fact,
            })),
        }),
        fact: Some(preview),
        found: found + source_found + payload_found,
        stubbed: 0,
    })
}

/// What the report shows of a memory before the store says what became of it.
fn preview_of(
    event_id: &str,
    capture: &'static str,
    upload: &Upload,
    fact: &NewFact,
    hint: Option<&'static str>,
) -> FactPreview {
    FactPreview {
        event_id: event_id.to_owned(),
        capture,
        fact_type: fact.fact_type,
        home: upload.home.clone(),
        t_created: fact.t_created,
        outcome: "created",
        hint,
        preview: redact::cut(&fact.content).to_owned(),
    }
}

/// Count each `[REDACTED:<rule>]` placeholder of `value`'s strings, by rule.
fn count_placeholders(value: &Value, counts: &mut BTreeMap<String, u64>) {
    const OPEN: &str = "[REDACTED:";
    match value {
        Value::String(text) => {
            let mut rest = text.as_str();
            while let Some(at) = rest.find(OPEN) {
                rest = &rest[at + OPEN.len()..];
                let end = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                    .unwrap_or(rest.len());
                if end > 0 && rest[end..].starts_with(']') {
                    *counts.entry(rest[..end].to_owned()).or_default() += 1;
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| count_placeholders(v, counts)),
        Value::Object(members) => members.values().for_each(|v| count_placeholders(v, counts)),
        _ => {}
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn time(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// The SHA-256 of the canonical JSON (`serde_json` sorts object keys) of every effect the upload
/// would write, the stored items' and the upload's own, and of everything its report shows
/// beside them (the body's digest, the reader's census, the refused items), but never a receipt
/// time: a dry run and its write agree when nothing changed, and any change of the body or of an
/// effect-bearing input changes it.
fn context_digest(
    upload: &Upload,
    source: &str,
    denylist: &[String],
    prepared: &[Prepared],
) -> Result<[u8; 32]> {
    let mut literals: Vec<&str> = denylist.iter().map(String::as_str).collect();
    literals.sort_unstable();
    let items = prepared
        .iter()
        .filter_map(|p| p.write.as_ref().ok())
        .map(effects)
        .collect::<Result<Vec<_>>>()?;
    let refused: Vec<Value> = prepared
        .iter()
        .filter_map(|p| {
            p.write
                .as_ref()
                .err()
                .map(|r| json!([p.summary.event_id, r]))
        })
        .collect();
    let context = json!({
        "kind": upload.kind,
        "source": source,
        "provenance": upload.provenance,
        "denylist": hex(&Sha256::digest(json!(literals).to_string().as_bytes())),
        "body": hex(&upload.body_sha256),
        "census": upload.census,
        "items": items,
        "refused": refused,
    });
    Ok(Sha256::digest(context.to_string().as_bytes()).into())
}

fn effects(item: &Keyed) -> Result<Value> {
    let (key, home, event, fact) = match &item.write {
        DeliveryWrite::Capture(capture) => (
            Some(capture.key.to_text()),
            &capture.home,
            &capture.event,
            capture.fact.as_ref(),
        ),
        DeliveryWrite::Memory(memory) => (None, &memory.home, &memory.event, Some(&memory.fact)),
        DeliveryWrite::Check | DeliveryWrite::Quarantine(_) => {
            return Err(MemoryError::Internal(
                "an imported item is a capture or a memory".into(),
            ));
        }
    };
    Ok(json!({
        "event_id": item.event_id,
        "digest": hex(&item.body_sha256),
        "key": key,
        "home": home,
        "event": {
            "timestamp": time(event.timestamp),
            "event_type": serde_json::to_value(&event.event_type)?,
            "source": event.source,
            "origin_node_id": event.origin_node_id,
            "session_id": event.session_id,
            "payload": event.payload,
        },
        "fact": fact.map(|fact| json!({
            "text": fact.content,
            "type": fact.fact_type.to_string(),
            "t_valid": fact.t_valid.map(time),
            "t_created": time(fact.t_created),
            "metadata": fact.metadata,
        })),
    }))
}
