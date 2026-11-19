//! The store-wide redaction totals `status` reports (#1024).

/// How many secrets the store's writes redacted, and how many tool calls they stored as
/// sensitive-path stubs, since the totals began (both start at 0 on a store from before #1024).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RedactionCounts {
    /// Secrets replaced by a `[REDACTED:<rule>]` placeholder in written text.
    pub redacted: u64,
    /// Tool calls stored as a stub because they touched a sensitive path.
    pub stubbed: u64,
}
