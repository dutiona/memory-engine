use chrono::{DateTime, Utc};
use clap::ValueEnum;

/// Output format for CLI commands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputFormat {
    /// Human-readable table (default)
    #[default]
    Table,
    /// Machine-readable JSON
    Json,
    /// Minimal plain text (one value per line, scriptable)
    Plain,
}

/// Truncate a string to `max_chars` characters, appending "..." if truncated.
/// Safe for multi-byte UTF-8 — never slices mid-character.
///
/// When `max_chars < 3` the suffix itself cannot fit; the returned string is at
/// most `max_chars` characters long (taken from the beginning of `s`) with no
/// ellipsis appended.
pub(crate) fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    // Not enough room for even the "..." suffix — just hard-truncate.
    if max_chars < 3 {
        return s.chars().take(max_chars).collect();
    }
    let truncated: String = s.chars().take(max_chars - 3).collect();
    format!("{truncated}...")
}

/// Parse an RFC 3339 datetime string into a `DateTime<Utc>`.
///
/// # Errors
///
/// Returns a `String` error message if the input is not a valid RFC 3339 timestamp.
pub fn parse_datetime(s: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| format!("invalid RFC 3339 datetime: {e}"))
}

/// Print a value as JSON to stdout.
///
/// # Errors
///
/// Returns an error if serialization or writing to stdout fails.
pub(crate) fn print_json<T: serde::Serialize>(value: &T) -> anyhow::Result<()> {
    serde_json::to_writer_pretty(std::io::stdout(), value)?;
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_str_no_truncation_when_short() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn truncate_str_appends_ellipsis() {
        let result = truncate_str("hello world", 8);
        assert_eq!(result, "hello...");
        assert_eq!(result.chars().count(), 8);
    }

    #[test]
    fn truncate_str_max_chars_less_than_3_no_overflow() {
        // max_chars=2: result must be at most 2 chars, no "..." appended
        let result = truncate_str("hello", 2);
        assert!(result.chars().count() <= 2, "got: {result:?}");

        let result0 = truncate_str("hello", 0);
        assert_eq!(result0, "");
    }

    #[test]
    fn truncate_str_exactly_3_chars() {
        // max_chars=3 with a longer string → "..."
        let result = truncate_str("abcdef", 3);
        assert_eq!(result, "...");
        assert_eq!(result.chars().count(), 3);
    }

    #[test]
    fn truncate_str_unicode_safe() {
        // Each emoji is multiple bytes but one char — must not panic or over-truncate
        let s = "😀😀😀😀😀";
        let result = truncate_str(s, 4);
        assert_eq!(result.chars().count(), 4);
        assert_eq!(result, "😀...");
    }

    #[test]
    fn parse_datetime_valid_rfc3339() {
        let dt = parse_datetime("2026-03-25T00:00:00Z").unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-03-25T00:00:00+00:00");
    }

    #[test]
    fn parse_datetime_invalid_returns_err() {
        assert!(parse_datetime("not-a-date").is_err());
        assert!(parse_datetime("2026-13-01T00:00:00Z").is_err());
    }
}
