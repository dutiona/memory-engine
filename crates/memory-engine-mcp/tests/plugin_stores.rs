//! The claude-mem and claude-memory plugin stores (#1117), driven with the checked-in synthetic
//! stores of `tests/fixtures/plugin-stores/`.

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use memory_engine_mcp::adapters::claude_code::{claude_mem, claude_memory};
use rusqlite::Connection;
use rusqlite::types::ValueRef;

const STORES: [&str; 2] = ["claude-mem.sql", "claude-memory.sql"];
const LOCAL: [&str; 4] = ["/home/", "/Users/", "/tmp/", "/var/folders/"];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugin-stores")
}

/// A fixture store, built in memory from its checked-in SQL.
fn store(name: &str) -> Connection {
    let sql = std::fs::read_to_string(fixtures_dir().join(name)).unwrap();
    let store = Connection::open_in_memory().unwrap();
    store.execute_batch(&sql).unwrap();
    store
}

/// Every TEXT value of every table of `store`.
fn texts(store: &Connection) -> Vec<String> {
    let tables: Vec<String> = store
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut texts = Vec::new();
    for table in tables {
        let mut stmt = store
            .prepare(&format!("SELECT * FROM \"{table}\""))
            .unwrap();
        let columns = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for i in 0..columns {
                if let ValueRef::Text(t) = row.get_ref(i).unwrap() {
                    texts.push(String::from_utf8(t.to_vec()).unwrap());
                }
            }
        }
    }
    texts
}

fn assert_public_safe(what: &str, text: &str) {
    for local in LOCAL {
        assert!(!text.contains(local), "{what} holds {local}");
    }
    assert_eq!(
        memory_engine::redact::scrub(text).unwrap(),
        text,
        "{what}: a redaction rule matches a fixture string"
    );
}

/// The fixture stores and their README hold no local path and no string a redaction rule would
/// change: every secret and canary is planted at run time.
#[test]
fn plugin_store_fixtures_are_public_safe() {
    for name in STORES {
        let texts = texts(&store(name));
        assert!(!texts.is_empty(), "{name} holds no row");
        for text in &texts {
            assert_public_safe(name, text);
        }
    }
    let readme = std::fs::read_to_string(fixtures_dir().join("README.md")).unwrap();
    assert_public_safe("README.md", &readme);
}

// ── The exporter ────────────────────────────────────────────────────────────────

fn sql(name: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(name)).unwrap()
}

/// A fixture store written to `path`.
fn file_store(path: &Path, name: &str) -> Connection {
    let store = Connection::open(path).unwrap();
    store.execute_batch(&sql(name)).unwrap();
    store
}

fn export(kind: &str, store: &Path, out: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_memory-engine-plugin-export"))
        .args([kind.as_ref(), store.as_os_str(), out.as_os_str()])
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Every file of `dir`, by name, with its bytes.
fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let path = e.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read(&path).unwrap())
        })
        .collect();
    files.sort();
    files
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// One file per project, named by its project's digest, private, listed in `index.tsv`, its
/// body the store module's export; a second run is byte-identical.
#[test]
fn the_exporter_writes_one_file_per_project() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conversations.db");
    let store = file_store(&path, "claude-memory.sql");
    let out = dir.path().join("out");

    let run = export("claude-memory", &path, &out);

    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "3 files, 23 rows\n");
    assert_eq!(mode(&out), 0o700);
    let slices = claude_memory::export(&store).unwrap();
    let name = |project: Option<&str>| {
        use sha2::{Digest, Sha256};
        let tag = project.map_or_else(
            || "none".to_owned(),
            |p| {
                Sha256::digest(p.as_bytes())[..6]
                    .iter()
                    .fold(String::new(), |mut hex, b| {
                        let _ = write!(hex, "{b:02x}");
                        hex
                    })
            },
        );
        format!("claude-memory-{tag}.jsonl")
    };
    let mut index = String::from("file\tproject\tsessions\trows\tbytes\n");
    let mut want: Vec<(String, Vec<u8>)> = Vec::new();
    for slice in &slices {
        let file = name(slice.project.as_deref());
        assert_eq!(mode(&out.join(&file)), 0o600, "{file}");
        let _ = writeln!(
            index,
            "{file}\t{}\t{}\t{}\t{}",
            slice.project.as_deref().unwrap_or_default(),
            slice.sessions,
            slice.rows,
            slice.body.len()
        );
        want.push((file, slice.body.clone()));
    }
    want.push(("index.tsv".to_owned(), index.into_bytes()));
    want.sort();
    let written = files(&out);
    assert_eq!(written, want);
    assert_eq!(mode(&out.join("index.tsv")), 0o600);

    let again = dir.path().join("again");
    assert!(export("claude-memory", &path, &again).status.success());
    assert_eq!(files(&again), written);
}

/// A WAL store's committed frames are read from its `-wal`, and neither file of the copy
/// changes; a copy whose header says WAL without a `-wal` beside it is refused, and with an
/// empty one it is the checkpointed store.
#[test]
fn the_exporter_reads_the_wal_and_refuses_a_copy_without_it() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let path = source.join("claude-mem.db");
    let writer = Connection::open(&path).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;")
        .unwrap();
    writer.execute_batch(&sql("claude-mem.sql")).unwrap();
    writer
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    writer
        .execute_batch(
            "INSERT INTO user_prompts (content_session_id, prompt_number, prompt_text, created_at,
               created_at_epoch) VALUES ('eef5665a-2f30-497d-87e7-ef64be1076a7', 3,
               'Only in the WAL.', '2026-03-25T11:00:00.000Z', 1774436400000);",
        )
        .unwrap();
    let copy = dir.path().join("copy");
    std::fs::create_dir(&copy).unwrap();
    for name in ["claude-mem.db", "claude-mem.db-wal"] {
        std::fs::copy(source.join(name), copy.join(name)).unwrap();
    }
    let copied = copy.join("claude-mem.db");
    let before = files(&copy);
    let in_source = files(&source);

    let out = dir.path().join("out");
    let run = export("claude-mem", &copied, &out);

    assert!(run.status.success(), "{}", stderr(&run));
    let bodies: String = files(&out)
        .into_iter()
        .filter(|(name, _)| name != "index.tsv")
        .map(|(_, body)| String::from_utf8(body).unwrap())
        .collect();
    assert!(bodies.contains("Only in the WAL."));
    for (name, bytes) in &before {
        assert_eq!(
            &std::fs::read(copy.join(name)).unwrap(),
            bytes,
            "{name} changed"
        );
    }
    assert_eq!(files(&source), in_source, "the source is untouched");
    assert!(files(&out).iter().all(|(name, _)| name == "index.tsv"
        || (name.starts_with("claude-mem-")
            && Path::new(name).extension().is_some_and(|e| e == "jsonl"))));
    drop(writer);

    std::fs::remove_file(copy.join("claude-mem.db-wal")).unwrap();
    let _ = std::fs::remove_file(copy.join("claude-mem.db-shm"));
    let refused = dir.path().join("refused");
    let run = export("claude-mem", &copied, &refused);
    assert_eq!(run.status.code(), Some(1));
    assert!(stderr(&run).contains("-wal"), "{}", stderr(&run));
    assert!(!refused.exists(), "a refusal writes nothing");

    std::fs::write(copy.join("claude-mem.db-wal"), b"").unwrap();
    let checkpointed = dir.path().join("checkpointed");
    let run = export("claude-mem", &copied, &checkpointed);
    assert!(run.status.success(), "{}", stderr(&run));
    let all: Vec<u8> = files(&checkpointed)
        .into_iter()
        .flat_map(|(_, b)| b)
        .collect();
    assert!(!String::from_utf8(all).unwrap().contains("Only in the WAL."));
    let want =
        claude_mem::export(&file_store(&dir.path().join("plain.db"), "claude-mem.sql")).unwrap();
    assert_eq!(files(&checkpointed).len(), want.len() + 1);
}

/// An output directory that exists is refused, and so is a store missing a column the export
/// reads, by its name; neither writes anything.
#[test]
fn the_exporter_refuses_a_used_directory_and_names_a_missing_column() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conversations.db");
    drop(file_store(&path, "claude-memory.sql"));
    let used = dir.path().join("used");
    std::fs::create_dir(&used).unwrap();
    let run = export("claude-memory", &path, &used);
    assert_eq!(run.status.code(), Some(1));
    assert_eq!(files(&used), Vec::new());

    let old = dir.path().join("old.db");
    let store = file_store(&old, "claude-memory.sql");
    store
        .execute_batch("ALTER TABLE messages DROP COLUMN is_notification;")
        .unwrap();
    drop(store);
    let out = dir.path().join("out");
    let run = export("claude-memory", &old, &out);
    assert_eq!(run.status.code(), Some(1));
    assert!(stderr(&run).contains("is_notification"), "{}", stderr(&run));
    assert!(!out.exists());
}
