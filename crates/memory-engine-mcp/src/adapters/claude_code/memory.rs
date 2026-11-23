//! Claude Code's native auto-memory files (#1026), in the format of `my-dotfiles#134`: one file
//! per upload, read by ADR-0009's memory-file rules into one memory.

use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use memory_engine::bootstrap::{MEMORY_INDEX, memory_item};
use memory_engine::types::lifecycle::{ImportItem, ImportMemory};
use sha2::{Digest, Sha256};

use crate::adapters::Mapped;

/// Read one memory file, named `name`, filed under the scope path `home`, with the copy's
/// modification time `mtime` as its last date (#1026).
///
/// `MEMORY.md` is skipped as `index`, and a file with an empty body (a frontmatter-only stub) as
/// `empty`. Otherwise the file is one [`ImportMemory`], typed and dated by
/// [`memory_item`]. Its event ID is `bf.m.` and the first 24 hex digits of the SHA-256 of its
/// home, a zero byte, its name, a zero byte and its body, so the same file re-sent is one
/// memory and the same body under two names or two projects is two; its digest is the
/// body's SHA-256.
///
/// # Errors
///
/// `"not UTF-8"` when the body is not UTF-8.
///
/// # Examples
///
/// ```
/// use chrono::Utc;
/// use memory_engine::types::lifecycle::ImportItem;
/// use memory_engine_mcp::adapters::claude_code::memory::read;
///
/// let body = b"---\nname: tests-first\nmetadata:\n  type: feedback\n---\nWrite the test first.\n";
/// let mapped = read("feedback_tests.md", "user:alice/project:p", body, Utc::now())?;
/// let ImportItem::Memory(memory) = &mapped.items[0] else { panic!("a memory") };
/// assert!(memory.event_id.starts_with("bf.m."));
/// assert_eq!(memory.item.text, "Write the test first.");
/// assert!(read("MEMORY.md", "user:alice/project:p", b"- [a](a.md)\n", Utc::now())?.items.is_empty());
/// # Ok::<(), &'static str>(())
/// ```
pub fn read(
    name: &str,
    home: &str,
    body: &[u8],
    mtime: DateTime<Utc>,
) -> Result<Mapped, &'static str> {
    let text = std::str::from_utf8(body).map_err(|_| "not UTF-8")?;
    let mut mapped = Mapped {
        lines: text.lines().filter(|l| !l.trim().is_empty()).count(),
        ..Mapped::default()
    };
    let item = memory_item(name, text, mtime);
    let Some(item) = item else {
        let reason = if name == MEMORY_INDEX {
            "index"
        } else {
            "empty"
        };
        mapped.skipped.insert(reason, 1);
        return Ok(mapped);
    };
    let mut key = Sha256::new();
    for part in [home.as_bytes(), b"\0", name.as_bytes(), b"\0", body] {
        key.update(part);
    }
    let event_id = key.finalize()[..12]
        .iter()
        .fold(String::from("bf.m."), |mut id, b| {
            let _ = write!(id, "{b:02x}");
            id
        });
    mapped.items.push(ImportItem::Memory(ImportMemory {
        event_id,
        body_sha256: Sha256::digest(body).into(),
        body_size: body.len() as u64,
        item,
    }));
    Ok(mapped)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::TimeZone;
    use memory_engine::types::FactType;
    use memory_engine::types::lifecycle::ImportItem;

    use super::*;

    const HOME: &str = "user:alice/project:p";

    fn fixture(name: &str) -> Vec<u8> {
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-code-memory");
        std::fs::read(dir.join(name)).unwrap()
    }

    fn mtime() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 20, 8, 0, 0).unwrap()
    }

    fn one(mapped: &Mapped) -> &memory_engine::types::lifecycle::ImportMemory {
        match mapped.items.as_slice() {
            [ImportItem::Memory(memory)] => memory,
            other => panic!("one memory, got {other:?}"),
        }
    }

    #[test]
    fn the_index_and_an_empty_memory_are_skipped() {
        let index = read("MEMORY.md", HOME, &fixture("MEMORY.md"), mtime()).unwrap();
        assert_eq!(index.items.len(), 0);
        assert_eq!(index.skipped.get("index"), Some(&1));
        let name = "project_empty_stub.md";
        let empty = read(name, HOME, &fixture(name), mtime()).unwrap();
        assert_eq!(empty.items.len(), 0);
        assert_eq!(empty.skipped.get("empty"), Some(&1));
    }

    #[test]
    fn a_memory_file_is_one_memory_with_its_type() {
        let name = "feedback_tests_first.md";
        let body = fixture(name);
        let mapped = read(name, HOME, &body, mtime()).unwrap();
        let memory = one(&mapped);
        assert_eq!(memory.item.fact_type, FactType::Procedural);
        assert!(memory.item.text.starts_with("Write the failing test first"));
        assert_eq!(memory.item.time, mtime(), "no other date: the copy's mtime");
        assert_eq!(memory.item.metadata["memory_type"], "feedback");
        assert_eq!(memory.body_sha256, <[u8; 32]>::from(Sha256::digest(&body)));
        let id = &memory.event_id;
        assert!(
            id.starts_with("bf.m.")
                && id.len() == 5 + 24
                && id[5..].bytes().all(|b| b.is_ascii_hexdigit()),
            "{id}"
        );

        let again = read(name, HOME, &body, mtime()).unwrap();
        assert_eq!(
            &one(&again).event_id,
            id,
            "the same file re-sent is one memory"
        );
        let twin = "feedback_red_before_green.md";
        let twin = read(twin, HOME, &fixture(twin), mtime()).unwrap();
        assert_eq!(one(&twin).item.text, memory.item.text, "one body");
        assert_ne!(&one(&twin).event_id, id, "two names are two memories");
        let elsewhere = read(name, "user:alice/project:q", &body, mtime()).unwrap();
        assert_ne!(
            &one(&elsewhere).event_id,
            id,
            "two projects are two memories"
        );

        let dated = "project_release_2026_03_14.md";
        let dated = read(dated, HOME, &fixture(dated), mtime()).unwrap();
        assert_eq!(
            one(&dated).item.time,
            Utc.with_ymd_and_hms(2026, 3, 14, 0, 0, 0).unwrap()
        );
        assert_eq!(one(&dated).item.fact_type, FactType::Episodic);
    }

    #[test]
    fn a_memory_file_that_is_not_utf8_is_refused() {
        assert_eq!(
            read("m.md", HOME, b"\xff\xfe", mtime()).unwrap_err(),
            "not UTF-8"
        );
    }
}
