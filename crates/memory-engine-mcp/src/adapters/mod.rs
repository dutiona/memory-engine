//! One adapter per harness (ADR-0020 §1): it turns one harness body into one neutral record,
//! and is chosen by a `match` on `ME-Harness`. There is no adapter trait until a second adapter
//! exists.
//!
//! A harness's backfill readers (#1026) live in its adapter too, one per `ME-Backfill-Kind`,
//! chosen by a `match` as well.

use std::collections::BTreeMap;

use memory_engine::types::lifecycle::ImportItem;

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
