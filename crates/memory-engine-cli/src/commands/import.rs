use std::path::{Path, PathBuf};

use memory_engine::{EngineConfig, MemoryEngine};

#[derive(clap::Args)]
pub struct ImportArgs {
    /// Path to JSON snapshot file (plain, .gz, or .zst — auto-detected)
    snapshot: PathBuf,

    /// Embedding dimension (required for compressed snapshots, auto-detected for plain JSON)
    #[arg(long)]
    embed_dim: Option<usize>,
}

pub async fn run(db: &Path, args: &ImportArgs) -> anyhow::Result<()> {
    if db.exists() {
        anyhow::bail!(
            "target database {} already exists — import requires a fresh path",
            db.display()
        );
    }

    let embed_dim = match args.embed_dim {
        Some(dim) => dim,
        None => peek_embed_dim_from_snapshot(&args.snapshot).map_err(|e| {
            anyhow::anyhow!("{e}\n\nHint: for compressed snapshots, pass --embed-dim explicitly")
        })?,
    };

    let config = EngineConfig::new(db.to_path_buf(), embed_dim);
    let mut engine = MemoryEngine::restore_json(&args.snapshot, &config)?;
    // Flush the restored projections to the sidecar so the next open of the freshly
    // imported DB does not rebuild the HNSW index from scratch (#728 review C).
    engine.close().await?;

    eprintln!(
        "Imported {} into {} (embed_dim={})",
        args.snapshot.display(),
        db.display(),
        embed_dim,
    );
    Ok(())
}

/// Maximum number of bytes scanned while looking for the snapshot's `embed_dim`.
///
/// In a well-formed snapshot `embed_dim` is the third field — it follows the
/// small `schema_version` (`u32`) and `storage_epoch` (`u16`) integers (see
/// `inspect::dump::stream_snapshot`), so its value always ends within ~80 bytes
/// of the start. 64 KiB is therefore ~800× the real worst case (ample headroom
/// for any future header growth) while still hard-bounding the scan against an
/// adversarial snapshot that omits `embed_dim` or buries it behind a huge
/// leading field — turning a would-be unbounded allocation (CWE-400 / CWE-770)
/// into a bounded read.
const MAX_HEADER_BYTES: u64 = 64 << 10; // 64 KiB

/// Peek at a JSON snapshot file's header to extract `embed_dim` without
/// loading the whole snapshot.
///
/// Scans at most [`MAX_HEADER_BYTES`] bytes and stops the moment the leading
/// `embed_dim` field is found — the bulk of the snapshot (`facts`, `summaries`,
/// `config`) is never read. Distinct from the `SQLite`-`config`-table reader
/// (`peek_embed_dim_from_db` in `crate::db`).
fn peek_embed_dim_from_snapshot(path: &Path) -> anyhow::Result<usize> {
    let file = std::fs::File::open(path)?;
    peek_embed_dim_from_reader(std::io::BufReader::new(file))
}

/// Extract `embed_dim` from the head of a JSON snapshot stream.
///
/// Split out from [`peek_embed_dim_from_snapshot`] so the parsing contract can
/// be unit-tested against adversarial byte streams without touching the
/// filesystem. The reader is capped at [`MAX_HEADER_BYTES`] and parsing stops at
/// the `embed_dim` field, so neither a multi-gigabyte document nor a single
/// oversized leading field can drive an unbounded read.
fn peek_embed_dim_from_reader(reader: impl std::io::Read) -> anyhow::Result<usize> {
    use serde::Deserializer as _;

    let capped = reader.take(MAX_HEADER_BYTES);
    let mut de = serde_json::Deserializer::from_reader(capped);

    // The sink captures `embed_dim` into `found` and stops reading the moment it
    // is seen, so the bulk of the snapshot is never parsed. Because the object is
    // abandoned mid-stream, `deserialize_map`'s closing-brace check then fails on
    // whatever follows the value — a trailing comma, the next key, EOF, or pure
    // garbage; ANY post-value error from the abandoned map traversal. That error
    // is *expected* and is ignored once we already hold the value. The underlying
    // parse error is only surfaced when `embed_dim` was never reached: a missing
    // field, malformed leading bytes, or a field buried past the byte cap.
    let mut found: Option<usize> = None;
    let parse = de.deserialize_map(EmbedDimSink { out: &mut found });

    if let Some(dim) = found {
        Ok(dim)
    } else {
        // `embed_dim` was never captured: surface the real parse error (malformed,
        // or the field is missing / buried past the cap) rather than the benign
        // trailing-comma error from the abandoned object.
        parse.map_err(|e| {
            anyhow::anyhow!(
                "failed to read embed_dim from snapshot header \
                 (scanned at most {MAX_HEADER_BYTES} bytes; malformed JSON, or \
                 embed_dim missing or pushed past the cap): {e}"
            )
        })?;
        anyhow::bail!("snapshot header object has no `embed_dim` field")
    }
}

/// Captures the snapshot's `embed_dim` field into `out` and stops reading the
/// object the moment it is found, so the (potentially huge) remainder of the
/// snapshot is never parsed.
struct EmbedDimSink<'a> {
    out: &'a mut Option<usize>,
}

impl<'de> serde::de::Visitor<'de> for EmbedDimSink<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a snapshot object containing an `embed_dim` field")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        while let Some(key) = map.next_key::<String>()? {
            if key == "embed_dim" {
                *self.out = Some(map.next_value::<usize>()?);
                return Ok(());
            }
            // Skip the value of any field preceding `embed_dim` without
            // materializing it (cheap for the small leading integers).
            map.next_value::<serde::de::IgnoredAny>()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The real snapshot header (see `inspect::dump::stream_snapshot`) is one JSON
    // object whose first fields are `schema_version`, `storage_epoch`, `embed_dim`,
    // followed by the bulk (`facts`, `summaries`, `config`).
    const VALID_HEADER_PREFIX: &str = r#"{"schema_version":5,"storage_epoch":0,"embed_dim":384,"#;

    #[test]
    fn reads_embed_dim_from_valid_snapshot() {
        let snapshot = format!("{VALID_HEADER_PREFIX}\"facts\":[],\"summaries\":[]}}");
        let dim = peek_embed_dim_from_reader(snapshot.as_bytes()).unwrap();
        assert_eq!(dim, 384);
    }

    #[test]
    fn stops_before_invalid_trailing_data() {
        // `embed_dim` is fully present, but everything after it is invalid JSON.
        // A correct peek returns 384 without ever parsing the poisoned tail; a
        // full-document parse blows up on it.
        let snapshot = format!("{VALID_HEADER_PREFIX}\"facts\":@@@NOT_JSON@@@");
        let dim = peek_embed_dim_from_reader(snapshot.as_bytes()).unwrap();
        assert_eq!(dim, 384);
    }

    #[test]
    fn rejects_header_larger_than_cap() {
        // Adversarial: a giant field *before* `embed_dim` forces an unbounded
        // read on a full-document parse. The peek must bound its scan and error.
        let giant = "x".repeat(2 * 1024 * 1024); // 2 MiB, well past the cap
        let snapshot = format!(r#"{{"junk":"{giant}","embed_dim":4}}"#);
        let result = peek_embed_dim_from_reader(snapshot.as_bytes());
        assert!(
            result.is_err(),
            "oversized header must be rejected, got {result:?}"
        );
    }

    #[test]
    fn errors_when_embed_dim_absent() {
        // A well-formed object that simply has no `embed_dim` must error cleanly
        // (the drain-to-close-then-bail path), not silently succeed.
        let snapshot = r#"{"schema_version":5,"storage_epoch":0,"facts":[]}"#;
        let result = peek_embed_dim_from_reader(snapshot.as_bytes());
        assert!(
            result.is_err(),
            "missing embed_dim must error, got {result:?}"
        );
    }

    #[test]
    fn stops_before_huge_valid_tail() {
        // The early-stop guarantee is about *volume*, not just validity: a huge but
        // perfectly valid `facts` array after `embed_dim` must not be parsed. We
        // prove it by making the tail larger than the byte cap — if the peek read
        // past `embed_dim` it would hit the cap and fail; stopping early succeeds.
        let huge_tail = "0,".repeat(1024 * 1024); // ~2 MiB of valid array elements
        let snapshot = format!("{VALID_HEADER_PREFIX}\"facts\":[{huge_tail}0]}}");
        let dim = peek_embed_dim_from_reader(snapshot.as_bytes()).unwrap();
        assert_eq!(dim, 384);
    }

    // --- adversarial invariants (locked in from the security/serde review) ---

    #[test]
    fn deep_nesting_before_embed_dim_does_not_overflow() {
        // A deeply-nested value *before* `embed_dim` must not blow the stack: the
        // skip path (`IgnoredAny`) is iterative in serde_json, and the byte cap
        // bounds the depth anyway. The header is rejected (it never reaches
        // `embed_dim` within the cap), but the point is it returns rather than
        // crashing with SIGSEGV.
        let snapshot = format!("{{\"a\":{}", "[".repeat(200_000));
        let result = peek_embed_dim_from_reader(snapshot.as_bytes());
        assert!(result.is_err(), "got {result:?}");
    }

    #[test]
    fn truncated_multibyte_char_does_not_panic() {
        // The byte cap truncates at an arbitrary byte boundary, possibly mid
        // multi-byte UTF-8 sequence. Parsing must error gracefully, never panic.
        // A long key padded with a multi-byte char ('é' = 2 bytes) crosses the cap.
        let cap = usize::try_from(MAX_HEADER_BYTES).unwrap();
        let padded_key = "é".repeat(cap); // 2 bytes/char ⇒ > cap, cut lands mid-char
        let snapshot = format!("{{\"{padded_key}\":1,\"embed_dim\":4}}");
        let result = peek_embed_dim_from_reader(snapshot.as_bytes());
        assert!(result.is_err(), "got {result:?}");
    }

    #[test]
    fn non_object_top_level_errors() {
        // Arrays, scalars, and null are not snapshot objects — each must error
        // cleanly (no panic, `found` stays None).
        for input in ["[1,2,3]", "5", "null", "\"x\""] {
            let result = peek_embed_dim_from_reader(input.as_bytes());
            assert!(result.is_err(), "{input:?} should error, got {result:?}");
        }
    }

    #[test]
    fn invalid_embed_dim_value_is_surfaced_not_masked() {
        // If `embed_dim` is present but its value is not a valid usize, the parse
        // error must propagate (the `?` fires before `found` is written), not be
        // swallowed by the ignore-trailing-error path.
        for bad in [r#""abc""#, "-1", "1.5"] {
            let snapshot = format!(r#"{{"embed_dim":{bad}}}"#);
            let result = peek_embed_dim_from_reader(snapshot.as_bytes());
            assert!(
                result.is_err(),
                "embed_dim={bad} should error, got {result:?}"
            );
        }
    }

    #[test]
    fn first_embed_dim_wins_on_duplicate_keys() {
        // Early-return means the first `embed_dim` is taken and the rest of the
        // object is never read — so a duplicate key cannot change the result.
        let snapshot = r#"{"embed_dim":4,"embed_dim":8}"#;
        let dim = peek_embed_dim_from_reader(snapshot.as_bytes()).unwrap();
        assert_eq!(dim, 4);
    }

    proptest::proptest! {
        /// Fuzz the snapshot-header peek (#433): arbitrary attacker-controlled
        /// bytes must never panic or hang — only ever Ok(dim) or a parse Err,
        /// bounded by the MAX_HEADER_BYTES cap.
        #[test]
        fn peek_embed_dim_never_panics(
            data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096)
        ) {
            let _ = peek_embed_dim_from_reader(data.as_slice());
        }
    }
}
