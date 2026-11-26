//! The lifecycle route's tables (#1025, ADR-0020 §3, §5): one delivery's receipt, event and
//! effects in one transaction, the session segments it steps, and the idle sweep.
//!
//! An upload's deliveries share one transaction, committed or rolled back whole (#1026).
//!
//! Every time written is the delivery's `received_at` or the sweep's `now`, as RFC 3339 with
//! microseconds and `Z`, so its text sorts like the instant it names. Every statement's error
//! goes through [`classify`], so a store that cannot take the write now is `Unavailable`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;

use super::activities::ActivityStore;
use super::events::{EventStore, event_type_to_str};
use super::scopes::ScopeStore;
use super::upcaster::UpcasterRegistry;
use super::{classify, embed_queue, parse_optional_timestamp, parse_timestamp, tombstones};
use me_types::error::{MemoryError, Result, StorageError};
use me_types::types::EventType;
use me_types::types::inspect::{LifecycleReceiptSnapshot, LifecycleSegmentSnapshot};
use me_types::types::lifecycle::{
    Applied, Batch, BatchMode, CaptureWrite, DeliveryWrite, EndedBy, Keyed, Kind, MemoryWrite,
    OrderKey, SegmentState, Step, preview_digest, step,
};

/// The idempotency key of one delivery, with the digest of its body and the time it arrived.
pub(crate) struct ReceiptKey<'a> {
    pub machine: &'a str,
    pub event_id: &'a str,
    pub body_sha256: &'a [u8; 32],
    pub received_at: DateTime<Utc>,
}

fn text(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// Record the delivery `key` and what `write` carries, in one transaction that takes the write
/// lock first (`BEGIN IMMEDIATE`): `Ok` means committed whole, `Err` means nothing changed.
///
/// # Errors
///
/// `Storage(Unavailable)` when the store is busy, locked, full, failing I/O or cannot be
/// opened; `Storage(Backend)` for any other SQL failure; the scope, event and fact stores'
/// own errors.
pub(crate) fn record_delivery(
    conn: &rusqlite::Connection,
    registry: &UpcasterRegistry,
    embed_dim: usize,
    key: &ReceiptKey<'_>,
    write: &DeliveryWrite,
) -> Result<Applied> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(classify)?;
    let applied = apply_one(&tx, registry, embed_dim, key, write, None)?;
    tx.commit().map_err(classify)?;
    Ok(applied)
}

/// Record an upload's `items` in one transaction that takes the write lock first, and commit it
/// only for a [`BatchMode::Write`] whose preview is the batch's and that met no collision.
///
/// Before any item, it reads once per session of the captures its first live event and the
/// other imports holding it on `machine`; each capture then yields as [`yields`] says.
///
/// # Errors
///
/// As [`record_delivery`]; nothing is written on any error.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_deliveries(
    conn: &rusqlite::Connection,
    registry: &UpcasterRegistry,
    embed_dim: usize,
    machine: &str,
    received_at: DateTime<Utc>,
    items: &[Keyed],
    context: &[u8; 32],
    mode: BatchMode,
) -> Result<Batch> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(classify)?;
    let sessions: BTreeSet<(&str, &str)> = items
        .iter()
        .filter_map(|item| match &item.write {
            DeliveryWrite::Capture(capture) => {
                Some((capture.session_id.as_str(), capture.event.source.as_str()))
            }
            _ => None,
        })
        .collect();
    let mut live_since = BTreeMap::new();
    let mut held_by: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut held = BTreeMap::new();
    for (session, source) in sessions {
        if !live_since.contains_key(session)
            && let Some(start) = first_live_event(&tx, machine, session)?
        {
            live_since.insert(session.to_owned(), start);
        }
        let holders = held_by.entry(session.to_owned()).or_default();
        if live_since.contains_key(session) {
            holders.insert("lifecycle".to_owned());
        }
        let mut kinds = BTreeSet::new();
        for (other, actor, kind) in other_imports(&tx, machine, session, source)? {
            holders.insert(other);
            if let (Some(actor), Some(kind)) = (actor, kind) {
                kinds.insert((actor, kind));
            }
        }
        held.insert((session, source), kinds);
    }
    held_by.retain(|_, holders| !holders.is_empty());
    let mut applied = Vec::with_capacity(items.len());
    for item in items {
        let key = ReceiptKey {
            machine,
            event_id: &item.event_id,
            body_sha256: &item.body_sha256,
            received_at,
        };
        let answer = match &item.write {
            DeliveryWrite::Capture(capture) => yields(
                capture,
                live_since.get(&capture.session_id),
                held.get(&(capture.session_id.as_str(), capture.event.source.as_str())),
            ),
            _ => None,
        };
        applied.push(apply_one(
            &tx,
            registry,
            embed_dim,
            &key,
            &item.write,
            answer,
        )?);
    }
    let preview = preview_digest(context, items, &applied);
    let committed = match mode {
        BatchMode::DryRun => false,
        BatchMode::Write { preview: reviewed } => {
            reviewed == preview && !applied.contains(&Applied::Collision)
        }
    };
    if committed {
        tx.commit().map_err(classify)?;
    } else {
        tx.rollback().map_err(classify)?;
    }
    Ok(Batch {
        committed,
        preview,
        applied,
        live_since,
        held_by,
    })
}

/// How long before a session's first live event a copy's capture already yields to it: the
/// offset between a plugin's hook clock and live capture's for the same event (#1117).
const LIVE_SLACK: TimeDelta = TimeDelta::seconds(60);

/// What a batch's capture writes instead of itself, given its session's first live event and
/// the `(actor, kind)`s other imports hold of the session; `None` when it is written.
///
/// A capture at or after the first live event is [`Applied::LiveOverlap`] (#1026). A plugin
/// store's copy yields from [`LIVE_SLACK`] before it, and otherwise is [`Applied::Superseded`]
/// when another import holds its actor and kind in the session (#1117). A copy never yields
/// to its own store's rows, and nothing yields to a copy.
fn yields(
    capture: &CaptureWrite,
    live_since: Option<&OrderKey>,
    held: Option<&BTreeSet<(String, String)>>,
) -> Option<Applied> {
    if !capture.copy {
        return live_since
            .is_some_and(|live| capture.key >= *live)
            .then_some(Applied::LiveOverlap);
    }
    if live_since.is_some_and(|live| capture.key.time >= live.time - LIVE_SLACK) {
        return Some(Applied::LiveOverlap);
    }
    let actor = capture.event.payload.get("actor").and_then(Value::as_str)?;
    let pair = (actor.to_owned(), capture.kind.as_str().to_owned());
    held.is_some_and(|held| held.contains(&pair))
        .then_some(Applied::Superseded)
}

/// Apply one delivery inside `tx`: the key is read first, and a held key answers
/// [`Applied::Duplicate`] (same digest) or [`Applied::Collision`] (another) and writes nothing.
/// Then a capture `answer`ed by [`yields`] writes nothing and answers that.
fn apply_one(
    tx: &Transaction<'_>,
    registry: &UpcasterRegistry,
    embed_dim: usize,
    key: &ReceiptKey<'_>,
    write: &DeliveryWrite,
    answer: Option<Applied>,
) -> Result<Applied> {
    let held: Option<Vec<u8>> = tx
        .query_row(
            "SELECT body_sha256 FROM lifecycle_receipts WHERE machine = ?1 AND event_id = ?2",
            params![key.machine, key.event_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(classify)?;
    if let Some(digest) = held {
        return Ok(if digest[..] == key.body_sha256[..] {
            Applied::Duplicate
        } else {
            Applied::Collision
        });
    }
    match write {
        DeliveryWrite::Check => Ok(Applied::Absent),
        // The capture contract does not keep it (#1171): nothing is applied, no key is held, and
        // the one durable fact is the store-wide skip total, raised in this same transaction.
        DeliveryWrite::Skip(reason) => {
            count_skip(tx, reason)?;
            Ok(Applied::Skipped(reason))
        }
        DeliveryWrite::Quarantine(record) => {
            insert_receipt(tx, key, None, None, Some(&record.to_string()))?;
            Ok(Applied::Quarantined)
        }
        DeliveryWrite::Capture(capture) => {
            answer.map_or_else(|| capture_event(tx, registry, embed_dim, key, capture), Ok)
        }
        DeliveryWrite::Memory(memory) => memory_event(tx, registry, embed_dim, key, memory),
    }
}

/// The live events of one session and machine; the payload reads are guarded by `json_valid`,
/// and `session_id` is the key of `idx_events_session`.
const LIVE_EVENTS: &str = "SELECT timestamp,
            CASE WHEN json_valid(payload) THEN json_extract(payload, '$.seq') END,
            CASE WHEN json_valid(payload) THEN json_extract(payload, '$.event_id') END
       FROM events
      WHERE session_id = ?1 AND source = 'lifecycle'
        AND CASE WHEN json_valid(payload) THEN json_extract(payload, '$.machine') END = ?2";

/// The other imports' events of one session and machine: their sources, and the actor and
/// kind each holds, read only when they are text. `session_id` is the key of
/// `idx_events_session`.
const OTHER_IMPORTS: &str = "SELECT DISTINCT source,
            CASE WHEN json_valid(payload) AND json_type(payload, '$.actor') = 'text'
                 THEN json_extract(payload, '$.actor') END,
            CASE WHEN json_valid(payload) AND json_type(payload, '$.kind') = 'text'
                 THEN json_extract(payload, '$.kind') END
       FROM events
      WHERE session_id = ?1 AND source NOT IN (?3, 'lifecycle')
        AND CASE WHEN json_valid(payload) THEN json_extract(payload, '$.machine') END = ?2";

/// An event's `source`, and its actor and kind when it has them.
type Holder = (String, Option<String>, Option<String>);

/// The holders of `session` and `machine` that neither live capture nor `own_source` wrote,
/// once each: an event with no actor or kind names its source alone.
fn other_imports(
    tx: &Transaction<'_>,
    machine: &str,
    session: &str,
    own_source: &str,
) -> Result<Vec<Holder>> {
    let mut stmt = tx.prepare_cached(OTHER_IMPORTS).map_err(classify)?;
    let rows = stmt
        .query_map(params![session, machine, own_source], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(classify)?;
    Ok(rows.collect::<rusqlite::Result<_>>().map_err(classify)?)
}

/// The order key of the first event of `session` that `machine` delivered live, if any.
fn first_live_event(
    tx: &Transaction<'_>,
    machine: &str,
    session: &str,
) -> Result<Option<OrderKey>> {
    let mut stmt = tx.prepare_cached(LIVE_EVENTS).map_err(classify)?;
    let mut rows = stmt.query(params![session, machine]).map_err(classify)?;
    let mut first: Option<OrderKey> = None;
    while let Some(row) = rows.next().map_err(classify)? {
        let time: String = row.get(0).map_err(classify)?;
        let seq: Option<i64> = row.get(1).map_err(classify)?;
        let event_id: Option<String> = row.get(2).map_err(classify)?;
        let key = OrderKey {
            time: parse_timestamp(&time).map_err(classify)?,
            seq: seq.and_then(|s| u32::try_from(s).ok()).unwrap_or_default(),
            event_id: event_id.unwrap_or_default(),
        };
        if first.as_ref().is_none_or(|f| key < *f) {
            first = Some(key);
        }
    }
    Ok(first)
}

fn insert_receipt(
    tx: &Transaction<'_>,
    key: &ReceiptKey<'_>,
    event_row: Option<i64>,
    call_event: Option<i64>,
    quarantine: Option<&str>,
) -> Result<()> {
    tx.execute(
        "INSERT INTO lifecycle_receipts
             (machine, event_id, body_sha256, received_at, event_row, call_event, quarantine)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            key.machine,
            key.event_id,
            &key.body_sha256[..],
            text(key.received_at),
            event_row,
            call_event,
            quarantine
        ],
    )
    .map_err(classify)?;
    Ok(())
}

fn capture_event(
    tx: &Transaction<'_>,
    registry: &UpcasterRegistry,
    embed_dim: usize,
    key: &ReceiptKey<'_>,
    capture: &CaptureWrite,
) -> Result<Applied> {
    let scopes = ScopeStore::new(tx);
    let leaf = scopes.ensure_path(&capture.home)?;
    let mut event = capture.event.clone();
    event.scope_id = leaf;
    let event_row = EventStore::new(tx, registry).insert(&event)?;
    if let Some((activity, window)) = &capture.activity {
        let mut activity = activity.clone();
        activity.scope_id = leaf;
        ActivityStore::new(tx).insert_or_dedup(&activity, *window)?;
    }
    let fact_row = match &capture.fact {
        Some(fact) if !tombstones::blocks(tx, &fact.content)? => {
            let mut fact = fact.clone();
            fact.scope_id = leaf;
            fact.source_event_id = Some(event_row);
            Some(embed_queue::queue(tx, &fact, embed_dim, key.received_at)?)
        }
        _ => None,
    };
    let call_event = match &capture.call {
        Some(call) => linked_call(tx, call, key.machine)?,
        None => None,
    };
    let seen = Seen {
        kind: capture.kind,
        main_turn: capture.main_turn,
        boundary: capture.boundary,
        final_message: capture.final_message,
        fact: fact_row,
    };
    step_segments(tx, key, &capture.session_id, &capture.key, &seen)?;
    insert_receipt(tx, key, Some(event_row), call_event, None)?;
    Ok(Applied::Stored {
        event: event_row,
        fact: fact_row,
        reinforced: false,
        chain: scopes.chain(leaf)?,
    })
}

/// An imported memory with its event and receipt: no segment step, no activity.
fn memory_event(
    tx: &Transaction<'_>,
    registry: &UpcasterRegistry,
    embed_dim: usize,
    key: &ReceiptKey<'_>,
    memory: &MemoryWrite,
) -> Result<Applied> {
    let scopes = ScopeStore::new(tx);
    let leaf = scopes.ensure_path(&memory.home)?;
    let mut event = memory.event.clone();
    event.scope_id = leaf;
    let event_row = EventStore::new(tx, registry).insert(&event)?;
    let mut fact = memory.fact.clone();
    fact.scope_id = leaf;
    fact.source_event_id = Some(event_row);
    let (fact_row, reinforced) =
        embed_queue::queue_or_reinforce(tx, &fact, embed_dim, key.received_at)?;
    insert_receipt(tx, key, Some(event_row), None, None)?;
    Ok(Applied::Stored {
        event: event_row,
        fact: fact_row,
        reinforced,
        chain: scopes.chain(leaf)?,
    })
}

/// The call record of one machine by call ID. Both payload reads are guarded by `json_valid`,
/// and the first is the expression of `idx_events_mcp_call`.
const LINKED_CALL: &str = "SELECT id FROM events
      WHERE source = 'mcp' AND event_type = ?3
        AND CASE WHEN json_valid(payload) THEN json_extract(payload, '$.call') END = ?1
        AND CASE WHEN json_valid(payload) THEN json_extract(payload, '$.machine') END = ?2
      ORDER BY id LIMIT 1";

/// The call record of `machine` whose payload names `call`; any other call line links nothing.
fn linked_call(tx: &Transaction<'_>, call: &str, machine: &str) -> Result<Option<i64>> {
    let id = tx
        .query_row(
            LINKED_CALL,
            params![call, machine, event_type_to_str(&EventType::MemoryOp)],
            |r| r.get(0),
        )
        .optional()
        .map_err(classify)?;
    Ok(id)
}

/// What the segment step needs of the delivered event.
struct Seen {
    kind: Kind,
    main_turn: bool,
    boundary: bool,
    final_message: bool,
    fact: Option<i64>,
}

/// A session's latest segment, as `step` and the update need it.
struct Latest {
    id: i64,
    number: i64,
    state: SegmentState,
    final_key: Option<String>,
    final_fact: Option<i64>,
}

fn latest_segment(tx: &Transaction<'_>, machine: &str, session_id: &str) -> Result<Option<Latest>> {
    let row = tx
        .query_row(
            "SELECT id, number, start_key, last_key, ended_by IS NOT NULL, final_key, final_fact
               FROM lifecycle_segments WHERE machine = ?1 AND session_id = ?2
              ORDER BY number DESC LIMIT 1",
            params![machine, session_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, bool>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                ))
            },
        )
        .optional()
        .map_err(classify)?;
    let Some((id, number, start, last, ended, final_key, final_fact)) = row else {
        return Ok(None);
    };
    let corrupt = || MemoryError::Internal(format!("lifecycle segment {id} holds a malformed key"));
    Ok(Some(Latest {
        id,
        number,
        state: SegmentState {
            start: OrderKey::from_text(&start).ok_or_else(corrupt)?,
            last: OrderKey::from_text(&last).ok_or_else(corrupt)?,
            ended,
        },
        final_key,
        final_fact,
    }))
}

fn end_segment(tx: &Transaction<'_>, id: i64, by: EndedBy, at: &str) -> Result<()> {
    tx.execute(
        "UPDATE lifecycle_segments SET ended_by = ?1, ended_at = ?2
          WHERE id = ?3 AND ended_by IS NULL",
        params![by.as_str(), at, id],
    )
    .map_err(classify)?;
    Ok(())
}

/// ADR-0020 §5: apply `step`'s verdict on `order` to the session's segments, then end the
/// segment the event is in when it is a session end. Every end is a conditional update, so a
/// segment ends once.
fn step_segments(
    tx: &Transaction<'_>,
    key: &ReceiptKey<'_>,
    session_id: &str,
    order: &OrderKey,
    seen: &Seen,
) -> Result<()> {
    let now = text(key.received_at);
    let latest = latest_segment(tx, key.machine, session_id)?;
    let verdict = step(latest.as_ref().map(|l| &l.state), order, seen.boundary);
    let order_text = order.to_text();
    let kind = seen.kind.as_str();
    let (final_key, final_fact) = (seen.final_message)
        .then_some((&order_text, seen.fact))
        .unzip();
    let segment = match (verdict, latest) {
        (Step::Late, latest) => {
            if let Some(open) = latest.filter(|l| !l.state.ended) {
                tx.execute(
                    "UPDATE lifecycle_segments SET received_at = ?1
                      WHERE id = ?2 AND ended_by IS NULL",
                    params![now, open.id],
                )
                .map_err(classify)?;
            }
            return Ok(());
        }
        (Step::Join, Some(latest)) => {
            let last = latest.state.last.clone().max(order.clone());
            let takes_final = final_key.is_some_and(|k| {
                latest
                    .final_key
                    .as_deref()
                    .is_none_or(|current| k.as_str() > current)
            });
            let (final_key, final_fact) = if takes_final {
                (final_key.cloned(), final_fact.flatten())
            } else {
                (latest.final_key, latest.final_fact)
            };
            tx.execute(
                "UPDATE lifecycle_segments
                    SET last_key = ?1, received_at = ?2,
                        counts = json_set(counts, '$.' || ?3,
                                          COALESCE(json_extract(counts, '$.' || ?3), 0) + 1),
                        main_turns = main_turns + ?4, final_key = ?5, final_fact = ?6
                  WHERE id = ?7",
                params![
                    last.to_text(),
                    now,
                    kind,
                    i64::from(seen.main_turn),
                    final_key,
                    final_fact,
                    latest.id
                ],
            )
            .map_err(classify)?;
            latest.id
        }
        (Step::Open { ends_latest }, latest) => {
            let number = latest.as_ref().map_or(1, |l| l.number + 1);
            if let (Some(latest), Some(by)) = (&latest, ends_latest) {
                end_segment(tx, latest.id, by, &now)?;
            }
            tx.execute(
                "INSERT INTO lifecycle_segments
                     (machine, session_id, number, start_key, last_key, received_at, counts,
                      main_turns, final_key, final_fact)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5, json_object(?6, 1), ?7, ?8, ?9)",
                params![
                    key.machine,
                    session_id,
                    number,
                    order_text,
                    now,
                    kind,
                    i64::from(seen.main_turn),
                    final_key,
                    final_fact.flatten()
                ],
            )
            .map_err(classify)?;
            tx.last_insert_rowid()
        }
        (Step::Join, None) => {
            return Err(MemoryError::Internal(
                "a session with no segment cannot be joined".to_owned(),
            ));
        }
    };
    if seen.kind == Kind::SessionEnd {
        end_segment(tx, segment, EndedBy::SessionEnd, &now)?;
    }
    Ok(())
}

/// The `config` key a skip total lives under (#1171): one per reason, so a report can name it.
const SKIP_PREFIX: &str = "skip.";

/// Raise `reason`'s skip total by one, in the delivery's own transaction.
fn count_skip(tx: &Transaction<'_>, reason: &'static str) -> Result<()> {
    let key = format!("{SKIP_PREFIX}{reason}");
    tx.execute(
        "INSERT INTO config (key, value) VALUES (?1, '1')
         ON CONFLICT(key) DO UPDATE SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT)",
        params![key],
    )
    .map_err(classify)?;
    Ok(())
}

/// Every skip total the store holds, by reason, in key order (#1171).
///
/// # Errors
///
/// `Storage(Backend)` on a read failure or a total that is not a count.
pub fn read_skips(tx: &Transaction<'_>) -> Result<BTreeMap<String, u64>> {
    let mut statement = tx
        .prepare("SELECT key, value FROM config WHERE substr(key, 1, 5) = ?1 ORDER BY key")
        .map_err(classify)?;
    let rows = statement
        .query_map(params![SKIP_PREFIX], |r| {
            let key: String = r.get(0)?;
            let value: String = r.get(1)?;
            Ok((key, value))
        })
        .map_err(classify)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(classify)?;
    let mut totals = BTreeMap::new();
    for (key, value) in rows {
        let total = value.parse::<u64>().map_err(StorageError::backend)?;
        totals.insert(key[SKIP_PREFIX.len()..].to_owned(), total);
    }
    Ok(totals)
}

/// How many quarantine records the store holds.
///
/// # Errors
///
/// `Storage(Backend)` on a read failure.
pub fn count_quarantined(conn: &rusqlite::Connection) -> Result<u64> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM lifecycle_receipts WHERE quarantine IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .map_err(classify)?;
    Ok(u64::try_from(count).unwrap_or_default())
}

/// Every receipt, in key order, one at a time (the JSON export).
///
/// # Errors
///
/// `Storage(Backend)` on a read failure, or what `f` returns.
pub fn for_each_receipt(
    conn: &rusqlite::Connection,
    f: &mut dyn FnMut(LifecycleReceiptSnapshot) -> Result<()>,
) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "SELECT machine, event_id, body_sha256, received_at, event_row, call_event, quarantine
               FROM lifecycle_receipts ORDER BY machine, event_id",
        )
        .map_err(classify)?;
    let mut rows = stmt.query([]).map_err(classify)?;
    while let Some(row) = rows.next().map_err(classify)? {
        let digest: Vec<u8> = row.get(2).map_err(classify)?;
        let received_at: String = row.get(3).map_err(classify)?;
        let quarantine: Option<String> = row.get(6).map_err(classify)?;
        f(LifecycleReceiptSnapshot {
            machine: row.get(0).map_err(classify)?,
            event_id: row.get(1).map_err(classify)?,
            body_sha256: digest.iter().fold(String::new(), |mut hex, b| {
                let _ = write!(hex, "{b:02x}");
                hex
            }),
            received_at: parse_timestamp(&received_at).map_err(classify)?,
            event_row: row.get(4).map_err(classify)?,
            call_event: row.get(5).map_err(classify)?,
            quarantine: quarantine
                .map(|text| serde_json::from_str(&text))
                .transpose()
                .map_err(StorageError::backend)?,
        })?;
    }
    Ok(())
}

/// Every segment, in id order, one at a time (the JSON export).
///
/// # Errors
///
/// `Storage(Backend)` on a read failure, or what `f` returns.
pub fn for_each_segment(
    conn: &rusqlite::Connection,
    f: &mut dyn FnMut(LifecycleSegmentSnapshot) -> Result<()>,
) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "SELECT id, machine, session_id, number, start_key, last_key, received_at, counts,
                    main_turns, final_key, final_fact, ended_by, ended_at
               FROM lifecycle_segments ORDER BY id",
        )
        .map_err(classify)?;
    let mut rows = stmt.query([]).map_err(classify)?;
    while let Some(row) = rows.next().map_err(classify)? {
        let received_at: String = row.get(6).map_err(classify)?;
        let counts: String = row.get(7).map_err(classify)?;
        let ended_at: Option<String> = row.get(12).map_err(classify)?;
        f(LifecycleSegmentSnapshot {
            id: row.get(0).map_err(classify)?,
            machine: row.get(1).map_err(classify)?,
            session_id: row.get(2).map_err(classify)?,
            number: row.get(3).map_err(classify)?,
            start_key: row.get(4).map_err(classify)?,
            last_key: row.get(5).map_err(classify)?,
            received_at: parse_timestamp(&received_at).map_err(classify)?,
            counts: serde_json::from_str(&counts).map_err(StorageError::backend)?,
            main_turns: row.get(8).map_err(classify)?,
            final_key: row.get(9).map_err(classify)?,
            final_fact: row.get(10).map_err(classify)?,
            ended_by: row.get(11).map_err(classify)?,
            ended_at: parse_optional_timestamp(ended_at.as_deref()).map_err(classify)?,
        })?;
    }
    Ok(())
}

/// End, as idle, every open segment whose latest event was received before `idle_before`, in one
/// transaction; returns how many.
///
/// # Errors
///
/// As [`record_delivery`].
pub(crate) fn end_idle_segments(
    conn: &rusqlite::Connection,
    idle_before: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<usize> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(classify)?;
    let ended = tx
        .execute(
            "UPDATE lifecycle_segments SET ended_by = ?1, ended_at = ?2
              WHERE ended_by IS NULL AND received_at < ?3",
            params![EndedBy::Idle.as_str(), text(now), text(idle_before)],
        )
        .map_err(classify)?;
    tx.commit().map_err(classify)?;
    Ok(ended)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use chrono::{Duration, TimeZone};
    use rusqlite::Connection;
    use serde_json::{Value, json};

    use super::*;
    use me_types::types::{FactType, NewEvent, NewFact};

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 21, 9, 0, 0).unwrap()
    }

    fn key(secs: i64, id: &str) -> OrderKey {
        OrderKey {
            time: t0() + Duration::seconds(secs),
            seq: 1,
            event_id: id.to_owned(),
        }
    }

    fn event(session: &str) -> NewEvent {
        NewEvent {
            timestamp: t0(),
            event_type: EventType::Interaction,
            payload: json!({}),
            source: "lifecycle".into(),
            session_id: Some(session.to_owned()),
            scope_id: 1,
            origin_node_id: "lifecycle".into(),
            sequence_id: 0,
            created_at: None,
        }
    }

    fn vectorless(content: &str) -> NewFact {
        NewFact::builder(content, Vec::new(), FactType::Episodic)
            .scope_id(1)
            .build()
    }

    /// One row of `lifecycle_segments`, without its ids.
    #[derive(Debug, Clone, PartialEq)]
    struct Segment {
        number: i64,
        start: String,
        last: String,
        counts: Value,
        main_turns: i64,
        final_key: Option<String>,
        final_fact: Option<i64>,
        ended_by: Option<String>,
        ended_at: Option<String>,
        received_at: String,
    }

    struct Harness {
        conn: Connection,
        registry: UpcasterRegistry,
        arrivals: Cell<i64>,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                conn: crate::test_util::setup_memory_db(),
                registry: UpcasterRegistry::new(),
                arrivals: Cell::new(0),
            }
        }

        /// Deliver one event of `session` as `laptop`, `secs` after `t0` on the client's clock;
        /// each delivery arrives one second after the previous one.
        fn deliver(
            &self,
            id: &str,
            session: &str,
            secs: i64,
            kind: Kind,
            tweak: impl FnOnce(&mut CaptureWrite),
        ) -> Applied {
            self.deliver_as("laptop", id, session, secs, kind, tweak)
        }

        fn deliver_as(
            &self,
            machine: &str,
            id: &str,
            session: &str,
            secs: i64,
            kind: Kind,
            tweak: impl FnOnce(&mut CaptureWrite),
        ) -> Applied {
            self.arrivals.set(self.arrivals.get() + 1);
            let mut capture = CaptureWrite {
                home: "p".to_owned(),
                event: event(session),
                session_id: session.to_owned(),
                key: key(secs, id),
                kind,
                main_turn: matches!(kind, Kind::Prompt | Kind::Stop),
                boundary: false,
                final_message: false,
                activity: None,
                fact: None,
                call: None,
                copy: false,
            };
            tweak(&mut capture);
            let receipt = ReceiptKey {
                machine,
                event_id: id,
                body_sha256: &[7; 32],
                received_at: t0() + Duration::seconds(10_000 + self.arrivals.get()),
            };
            record_delivery(
                &self.conn,
                &self.registry,
                4,
                &receipt,
                &DeliveryWrite::Capture(Box::new(capture)),
            )
            .unwrap()
        }

        fn segments(&self, session: &str) -> Vec<Segment> {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT number, start_key, last_key, counts, main_turns, final_key, final_fact,
                            ended_by, ended_at, received_at
                       FROM lifecycle_segments WHERE session_id = ?1 ORDER BY number",
                )
                .unwrap();
            stmt.query_map([session], |r| {
                Ok(Segment {
                    number: r.get(0)?,
                    start: r.get(1)?,
                    last: r.get(2)?,
                    counts: serde_json::from_str(&r.get::<_, String>(3)?).unwrap(),
                    main_turns: r.get(4)?,
                    final_key: r.get(5)?,
                    final_fact: r.get(6)?,
                    ended_by: r.get(7)?,
                    ended_at: r.get(8)?,
                    received_at: r.get(9)?,
                })
            })
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect()
        }
    }

    fn final_message(fact: &str) -> impl FnOnce(&mut CaptureWrite) {
        let fact = vectorless(fact);
        move |c| {
            c.final_message = true;
            c.fact = Some(fact);
        }
    }

    fn fact_of(applied: &Applied) -> i64 {
        match applied {
            Applied::Stored {
                fact: Some(fact), ..
            } => *fact,
            other => panic!("expected a stored fact, got {other:?}"),
        }
    }

    #[test]
    fn segments_follow_the_order_key() {
        let h = Harness::new();
        let boundary = |c: &mut CaptureWrite| c.boundary = true;

        // Open, then join: counts, main turns, the last key and the final message.
        h.deliver("a", "s", 0, Kind::SessionStart, boundary);
        h.deliver("b", "s", 10, Kind::Prompt, |_| {});
        let done = h.deliver("c", "s", 30, Kind::Stop, final_message("done"));
        let done = fact_of(&done);
        let seg = &h.segments("s")[0];
        assert_eq!(seg.start, key(0, "a").to_text());
        assert_eq!(seg.last, key(30, "c").to_text());
        assert_eq!(
            seg.counts,
            json!({ "session_start": 1, "prompt": 1, "stop": 1 })
        );
        assert_eq!(seg.main_turns, 2);
        assert_eq!(
            seg.final_key.as_deref(),
            Some(key(30, "c").to_text().as_str())
        );
        assert_eq!(seg.final_fact, Some(done));
        assert_eq!(seg.ended_by, None);

        // Out of order inside the range: joins; a final message with a smaller key is stored
        // and its memory made, but the segment's final message stays the greater key's.
        h.deliver("d", "s", 20, Kind::Prompt, |_| {});
        h.deliver("e", "s", 25, Kind::Stop, final_message("earlier"));
        let seg = &h.segments("s")[0];
        assert_eq!(
            seg.last,
            key(30, "c").to_text(),
            "the last key is the greatest"
        );
        assert_eq!(seg.main_turns, 4);
        assert_eq!(
            seg.final_key.as_deref(),
            Some(key(30, "c").to_text().as_str())
        );
        assert_eq!(seg.final_fact, Some(done));

        // A boundary after the last event ends the segment and opens the next.
        h.deliver("f", "s", 40, Kind::SessionStart, boundary);
        let ended_by_start = h.segments("s")[0].clone();
        assert_eq!(ended_by_start.ended_by.as_deref(), Some("session_start"));
        assert!(ended_by_start.ended_at.is_some());
        assert_eq!(h.segments("s")[1].start, key(40, "f").to_text());

        // A session end ends the segment it joins, with no main turn: no checkpoint.
        h.deliver("g", "s", 50, Kind::SessionEnd, |_| {});
        let ended_by_end = h.segments("s")[1].clone();
        assert_eq!(ended_by_end.ended_by.as_deref(), Some("session_end"));
        assert_eq!(
            ended_by_end.last,
            key(50, "g").to_text(),
            "the end is the last key"
        );
        assert_eq!(ended_by_end.main_turns, 0);

        // A late event is stored and changes no row, and a late resume opens nothing.
        let late = h.deliver("h", "s", 45, Kind::Prompt, |_| {});
        assert!(matches!(late, Applied::Stored { .. }));
        h.deliver("i", "s", 5, Kind::SessionStart, boundary);
        let rows = h.segments("s");
        assert_eq!(rows, [ended_by_start, ended_by_end.clone()]);

        // After an ended segment, the next event opens another.
        h.deliver("j", "s", 60, Kind::Prompt, |_| {});
        let rows = h.segments("s");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1], ended_by_end);
        assert_eq!(
            (
                rows[2].number,
                rows[2].main_turns,
                rows[2].ended_by.as_deref()
            ),
            (3, 1, None)
        );
    }

    #[test]
    fn a_client_gap_over_the_threshold_ends_the_segment_as_a_gap() {
        let h = Harness::new();
        h.deliver("a", "g", 0, Kind::Prompt, |_| {});
        h.deliver("b", "g", 1800, Kind::Prompt, |_| {});
        assert_eq!(h.segments("g").len(), 1, "exactly 30 minutes joins");

        h.deliver("c", "g", 1800 + 1801, Kind::Prompt, |_| {});

        let rows = h.segments("g");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].ended_by.as_deref(), Some("gap"));
        assert_eq!(rows[1].start, key(3601, "c").to_text());
    }

    #[test]
    fn a_late_event_only_refreshes_an_open_segment_receipt_time() {
        let h = Harness::new();
        h.deliver("a", "o", 100, Kind::Prompt, |_| {});
        let before = h.segments("o")[0].clone();

        h.deliver("b", "o", 50, Kind::Prompt, |_| {});

        let after = h.segments("o")[0].clone();
        assert!(after.received_at > before.received_at);
        assert_eq!(
            Segment {
                received_at: before.received_at.clone(),
                ..after
            },
            before
        );
    }

    fn call_record(h: &Harness, source: &str, call: &str, machine: &str) -> i64 {
        let mut record = event("");
        record.event_type = EventType::MemoryOp;
        record.source = source.to_owned();
        record.payload = json!({ "call": call, "tool": "recall", "machine": machine });
        EventStore::new(&h.conn, &h.registry)
            .insert(&record)
            .unwrap()
    }

    fn linked(h: &Harness, machine: &str, id: &str, call: &str) -> Option<i64> {
        h.deliver_as(machine, id, "s", 0, Kind::ToolUse, |c| {
            c.call = Some(call.to_owned());
        });
        h.conn
            .query_row(
                "SELECT call_event FROM lifecycle_receipts WHERE machine = ?1 AND event_id = ?2",
                [machine, id],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn a_call_links_only_the_same_machines_record() {
        let h = Harness::new();
        let (laptop, desktop) = ("aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb");
        let laptop_row = call_record(&h, "mcp", laptop, "laptop");
        let desktop_row = call_record(&h, "mcp", desktop, "desktop");
        call_record(&h, "lifecycle", "dddddddddddddddd", "laptop");
        // A malformed payload never fails an insert or a lookup.
        h.conn
            .execute(
                "INSERT INTO events (timestamp, event_type, payload, source)
                 VALUES ('t', 'memory_op', 'not json', 'mcp')",
                [],
            )
            .unwrap();

        assert_eq!(linked(&h, "laptop", "e1", laptop), Some(laptop_row));
        assert_eq!(linked(&h, "desktop", "e2", desktop), Some(desktop_row));
        assert_eq!(
            linked(&h, "desktop", "e3", laptop),
            None,
            "another machine's call"
        );
        assert_eq!(
            linked(&h, "laptop", "e4", desktop),
            None,
            "another machine's call"
        );
        assert_eq!(
            linked(&h, "laptop", "e5", "cccccccccccccccc"),
            None,
            "a forged line"
        );
        assert_eq!(
            linked(&h, "laptop", "e6", "dddddddddddddddd"),
            None,
            "not an MCP record"
        );
    }

    /// Every row of every table, as text, by table.
    fn tables(conn: &Connection) -> Vec<(String, Vec<String>)> {
        let names: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect();
        names
            .into_iter()
            .map(|name| {
                let mut stmt = conn.prepare(&format!("SELECT * FROM \"{name}\"")).unwrap();
                let columns = stmt.column_count();
                let rows = stmt
                    .query_map([], |r| {
                        Ok((0..columns)
                            .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                            .collect::<Vec<_>>()
                            .join("|"))
                    })
                    .unwrap()
                    .map(std::result::Result::unwrap)
                    .collect();
                (name, rows)
            })
            .collect()
    }

    /// A dry run leaves every table as it was, the scope tree and the queue included, and a
    /// write of the same batch then commits exactly what the dry run showed.
    #[test]
    fn a_dry_run_leaves_every_table_as_it_was() {
        let h = Harness::new();
        h.deliver("live", "s", 100, Kind::Prompt, |c| {
            c.event.payload = json!({ "machine": "laptop", "seq": 1, "event_id": "live" });
        });
        let mut memory_event = event("s");
        memory_event.session_id = None;
        let items = vec![
            Keyed {
                event_id: "a".into(),
                body_sha256: [1; 32],
                write: DeliveryWrite::Capture(Box::new(CaptureWrite {
                    home: "new/home".into(),
                    event: event("s"),
                    session_id: "s".into(),
                    key: key(0, "a"),
                    kind: Kind::Stop,
                    main_turn: true,
                    boundary: false,
                    final_message: true,
                    activity: Some((
                        me_types::types::NewActivity {
                            session_id: "s".into(),
                            tool_name: "Bash".into(),
                            args_hash: "h".into(),
                            args: json!({}),
                            result_summary: None,
                            outcome_class: me_types::types::OutcomeClass::Success,
                            timestamp: t0(),
                            scope_id: 1,
                            machine: Some("laptop".into()),
                        },
                        300,
                    )),
                    fact: Some(vectorless("made")),
                    call: None,
                    copy: false,
                })),
            },
            Keyed {
                event_id: "m".into(),
                body_sha256: [1; 32],
                write: DeliveryWrite::Memory(Box::new(MemoryWrite {
                    home: "other/home".into(),
                    event: memory_event,
                    fact: vectorless("remembered"),
                })),
            },
            Keyed {
                event_id: "late".into(),
                body_sha256: [1; 32],
                write: DeliveryWrite::Capture(Box::new(CaptureWrite {
                    key: key(200, "late"),
                    ..prompt_write("s", 200, "late")
                })),
            },
        ];
        let before = tables(&h.conn);
        let at = t0() + Duration::days(1);

        let dry = record_deliveries(
            &h.conn,
            &h.registry,
            4,
            "laptop",
            at,
            &items,
            &[0; 32],
            BatchMode::DryRun,
        )
        .unwrap();

        assert!(!dry.committed);
        assert_eq!(tables(&h.conn), before);
        assert!(matches!(dry.applied[2], Applied::LiveOverlap));
        let write = record_deliveries(
            &h.conn,
            &h.registry,
            4,
            "laptop",
            at,
            &items,
            &[0; 32],
            BatchMode::Write {
                preview: dry.preview,
            },
        )
        .unwrap();
        assert!(write.committed);
        assert_eq!(
            write.applied, dry.applied,
            "the same outcomes, row ids included"
        );
        assert_ne!(tables(&h.conn), before);
    }

    fn prompt_write(session: &str, secs: i64, id: &str) -> CaptureWrite {
        CaptureWrite {
            home: "p".to_owned(),
            event: event(session),
            session_id: session.to_owned(),
            key: key(secs, id),
            kind: Kind::Prompt,
            main_turn: true,
            boundary: false,
            final_message: false,
            activity: None,
            fact: None,
            call: None,
            copy: false,
        }
    }

    #[test]
    fn the_live_lookup_reads_the_session_index() {
        let conn = crate::test_util::setup_memory_db();
        let plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {LIVE_EVENTS}"))
            .unwrap()
            .query_map(["s", "laptop"], |r| r.get(3))
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect();
        assert!(
            plan.iter().any(|step| step.contains("idx_events_session")),
            "{plan:?}"
        );
    }

    #[test]
    fn the_origin_lookup_reads_the_session_index() {
        let conn = crate::test_util::setup_memory_db();
        let plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {OTHER_IMPORTS}"))
            .unwrap()
            .query_map(["s", "laptop", "claude-mem"], |r| r.get(3))
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect();
        assert!(
            plan.iter().any(|step| step.contains("idx_events_session")),
            "{plan:?}"
        );
    }

    #[test]
    fn the_call_lookup_reads_its_expression_index() {
        let conn = crate::test_util::setup_memory_db();
        let plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {LINKED_CALL}"))
            .unwrap()
            .query_map(["x", "laptop", "MemoryOp"], |r| r.get(3))
            .unwrap()
            .map(std::result::Result::unwrap)
            .collect();
        assert!(
            plan.iter().any(|step| step.contains("idx_events_mcp_call")),
            "{plan:?}"
        );
    }
}
