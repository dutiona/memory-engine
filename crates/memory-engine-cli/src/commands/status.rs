use std::fmt::Write as _;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::db::ensure_store_exists;
use crate::output::{self, OutputFormat};

/// Machine-readable embedding-queue report; an absent value is `null`.
#[derive(Serialize)]
struct StatusReport {
    waiting: u64,
    oldest_age_secs: Option<i64>,
    failed: u64,
    paused_since: Option<String>,
    pause_error: Option<String>,
    last_success_at: Option<String>,
    last_error_at: Option<String>,
    last_error: Option<String>,
}

/// Report the deferred-embedding queue (#1021).
///
/// Reads through one read-only connection: it builds no engine, takes no store lock and
/// never migrates, so it runs beside a live server. Exits 0 in every queue state; the
/// pause alarm is in the output.
// Sync, like `schema`: a read-only query on the DB file, no async engine call.
pub fn run(db: &Path, format: OutputFormat) -> anyhow::Result<()> {
    ensure_store_exists(db)?;
    let status = memory_engine::read_embed_queue_status(db)?;
    let time = |t: Option<DateTime<Utc>>| t.map(|t| t.to_rfc3339());
    let report = StatusReport {
        waiting: status.waiting,
        oldest_age_secs: status
            .oldest_enqueued_at
            .map(|t| (Utc::now() - t).num_seconds().max(0)),
        failed: status.failed,
        paused_since: time(status.paused_since),
        pause_error: status.pause_error,
        last_success_at: time(status.last_success_at),
        last_error_at: time(status.last_error_at),
        last_error: status.last_error,
    };

    match format {
        OutputFormat::Json => output::print_json(&report)?,
        OutputFormat::Table => print_table(&report),
        OutputFormat::Plain => {
            let value = |v: Option<&str>| v.map(escape_controls).unwrap_or_default();
            println!("waiting={}", report.waiting);
            println!(
                "oldest_age_secs={}",
                report
                    .oldest_age_secs
                    .map(|s| s.to_string())
                    .unwrap_or_default()
            );
            println!("failed={}", report.failed);
            println!("paused_since={}", value(report.paused_since.as_deref()));
            println!("pause_error={}", value(report.pause_error.as_deref()));
            println!(
                "last_success_at={}",
                value(report.last_success_at.as_deref())
            );
            println!("last_error_at={}", value(report.last_error_at.as_deref()));
            println!("last_error={}", value(report.last_error.as_deref()));
        }
    }
    Ok(())
}

fn print_table(report: &StatusReport) {
    let oldest = report
        .oldest_age_secs
        .map(|secs| format!(", oldest {}", age(secs)))
        .unwrap_or_default();
    println!(
        "embedding queue: {} waiting{oldest}; {} failed",
        report.waiting, report.failed
    );
    let error = |e: Option<&str>| escape_controls(e.unwrap_or_default());
    match &report.paused_since {
        Some(since) => println!(
            "paused: since {since}: {}",
            error(report.pause_error.as_deref())
        ),
        None => println!("paused: no"),
    }
    println!(
        "last embed success: {}",
        report.last_success_at.as_deref().unwrap_or("never")
    );
    match &report.last_error_at {
        Some(at) => println!(
            "last embed error: {at}: {}",
            error(report.last_error.as_deref())
        ),
        None => println!("last embed error: never"),
    }
}

/// ADR-0020 §10's age: the largest whole unit of minutes, hours or days.
fn age(secs: i64) -> String {
    match secs {
        s if s >= 86_400 => format!("{}d", s / 86_400),
        s if s >= 3_600 => format!("{}h", s / 3_600),
        s => format!("{}m", s / 60),
    }
}

/// An embedder's error body reaches the terminal: escape its control characters.
fn escape_controls(text: &str) -> String {
    text.chars()
        .fold(String::with_capacity(text.len()), |mut out, c| {
            if c.is_control() {
                let _ = write!(out, "{}", c.escape_default());
            } else {
                out.push(c);
            }
            out
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_uses_the_largest_whole_unit() {
        let ages: Vec<String> = [0, 59, 60, 3_599, 3_600, 86_399, 86_400, 200_000]
            .into_iter()
            .map(age)
            .collect();
        assert_eq!(ages, ["0m", "0m", "1m", "59m", "1h", "23h", "1d", "2d"]);
    }
}
