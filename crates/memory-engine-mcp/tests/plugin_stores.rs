//! The claude-mem and claude-memory plugin stores (#1117), driven with the checked-in synthetic
//! stores of `tests/fixtures/plugin-stores/`.

use std::path::PathBuf;

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
