//! One adapter per harness (ADR-0020 §1): it turns one harness body into one neutral record,
//! and is chosen by a `match` on `ME-Harness`. There is no adapter trait until a second adapter
//! exists.
//!
//! A harness's backfill readers (#1026) live in its adapter too, one per `ME-Backfill-Kind`,
//! chosen by a `match` as well.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Timelike, Utc};
use memory_engine::bootstrap::read_json_lines;
use memory_engine::types::lifecycle::{ImportEvent, ImportItem, NeutralRecord};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub mod claude_code;

/// What a backfill reader made of one uploaded file: its items, and the census of what it did
/// not import.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mapped {
    /// The items, in file order.
    pub items: Vec<ImportItem>,
    /// The file's non-empty lines.
    pub lines: usize,
    /// The lines that are not a JSON object.
    pub malformed: usize,
    /// What was read and not imported, by reason: a fixed phrase each.
    pub skipped: BTreeMap<&'static str, usize>,
    /// The tool calls whose result is not in the file yet: a later upload imports them.
    pub pending: usize,
    /// The turns with no completed message: interrupted, killed, or copied mid-turn.
    pub incomplete_turns: usize,
}

/// The records one millisecond may hold: `seq` is the millisecond times this plus the rank,
/// so it stays within ADR-0020's six digits.
const PER_MILLISECOND: u32 = 1000;

/// A record, in file order, before its order key.
pub(crate) struct Raw {
    pub(crate) line: usize,
    pub(crate) time: DateTime<Utc>,
    pub(crate) event_id: String,
    pub(crate) record: NeutralRecord,
}

/// `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`, ADR-0020 §3's event ID.
pub(crate) fn is_event_id(id: &str) -> bool {
    let b = id.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// A body's JSON objects, up to the entry limit: the objects with their line, and the
/// lines that are not objects, else the phrase naming the limit.
#[expect(
    clippy::type_complexity,
    reason = "the objects with their lines, and the malformed count"
)]
pub(crate) fn json_objects(
    body: &[u8],
    max_entries: usize,
    over_the_limit: &'static str,
) -> Result<(Vec<(usize, Value)>, usize), &'static str> {
    let (objects, malformed, truncated) =
        read_json_lines(body, memory_engine::BACKFILL_BODY_BYTES as u64, max_entries);
    if truncated {
        return Err(over_the_limit);
    }
    Ok((objects, malformed))
}

/// A reader's records, in file order, as events: the per-millisecond rank map, the
/// "over 1,000 records in one millisecond" skip counted into `skipped`, and each record
/// keyed by its time, its rank and the digest of its paired value.
pub(crate) fn keyed(
    records: Vec<(Raw, Value)>,
    skipped: &mut BTreeMap<&'static str, usize>,
) -> Vec<ImportItem> {
    let mut items = Vec::with_capacity(records.len());
    let mut ranks: HashMap<(i64, u32), u32> = HashMap::new();
    for (raw, value) in records {
        let millisecond = raw.time.timestamp_subsec_millis().min(PER_MILLISECOND - 1);
        let rank = ranks
            .entry((raw.time.timestamp(), millisecond))
            .or_default();
        if *rank >= PER_MILLISECOND {
            *skipped
                .entry("over 1,000 records in one millisecond")
                .or_default() += 1;
            continue;
        }
        let seq = millisecond * PER_MILLISECOND + *rank;
        *rank += 1;
        let canonical = value.to_string();
        items.push(ImportItem::Event(ImportEvent {
            event_id: raw.event_id,
            time: raw.time.with_nanosecond(0).unwrap_or(raw.time),
            seq,
            body_sha256: Sha256::digest(canonical.as_bytes()).into(),
            body_size: canonical.len() as u64,
            line: raw.line,
            record: raw.record,
        }));
    }
    items
}
