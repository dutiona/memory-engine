//! #551 — native `.md` memory-directory import.
//!
//! Exercises `MemoryEngine::bootstrap_memory_directory` end-to-end against a
//! temp tree of native memory files, asserting the four properties the
//! acceptance criteria call out:
//!   1. parse + type-routing — frontmatter `type:` maps to the right `FactType`;
//!   2. backdating — `t_created` comes from the frontmatter date (or mtime),
//!      never `Utc::now()`;
//!   3. redaction-before-write — a planted secret never reaches the store;
//!   4. idempotency — a re-run creates 0 facts and reinforces instead;
//!   5. sources read-only — every `.md` byte is unchanged after a run.
//!
//! Hermetic: in-memory engine + zero-vector embedder. No network, no Ollama.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Datelike;
use memory_engine::types::FactType;
use memory_engine::{
    BootstrapConfig, EmbeddingFingerprint, EmbeddingProvider, MemoryEngine, MemoryError,
};

/// Zero-vector embedder (dim 4) — retrieval is irrelevant here.
struct TestEmbedder;
impl EmbeddingProvider for TestEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>, MemoryError> {
        Ok(vec![0.0; 4])
    }
    fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint::new("mock", "test", 4)
    }
}

/// A planted AWS-access-key literal (matches the `aws-access-key` detector).
const PLANTED_SECRET: &str = "AKIAIOSFODNN7EXAMPLE";

/// Write `name` under `dir` with `contents`; create parent dirs as needed.
fn write_file(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, contents).unwrap();
    path
}

/// Recursively read every file's bytes under `dir` into `out`.
fn walk_bytes(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk_bytes(&path, out);
        } else {
            out.insert(path.clone(), fs::read(&path).unwrap());
        }
    }
}

/// Snapshot every file's raw bytes under `dir` (recursive) for a read-only proof.
fn snapshot_bytes(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    walk_bytes(dir, &mut out);
    out
}

fn build_corpus(root: &Path) {
    write_file(
        root,
        "user_pref.md",
        "---\nname: user-pref\ndescription: editor preference\nmetadata:\n  type: user\n---\nThe user prefers tabs over spaces.\n",
    );
    write_file(
        root,
        "feedback_tdd.md",
        "---\nname: tdd\nmetadata:\n  type: feedback\n---\nAlways write a failing test first.\n",
    );
    // project type + explicit backdate + a planted secret in the body.
    write_file(
        root,
        "project_handoff.md",
        &format!(
            "---\nname: handoff\nvalid_from: 2024-05-01\nmetadata:\n  type: project\n---\nStream B resumes at issue 551. Leaked token {PLANTED_SECRET} must be scrubbed.\n"
        ),
    );
    write_file(
        root,
        "reference_spec.md",
        "---\nname: spec\nmetadata:\n  type: reference\n---\nThe four-layer architecture spec lives in the wiki.\n",
    );
    // No frontmatter — whole body is the fact (the MEMORY index file).
    write_file(root, "MEMORY.md", "# Memory Index\n- pointer to a fact\n");
    // Recursion: a memory in a nested project subdir.
    write_file(
        root,
        "sub/nested.md",
        "---\nmetadata:\n  type: user\n---\nA nested memory fact.\n",
    );
    // Frontmatter-only stub → empty body → skipped.
    write_file(root, "empty.md", "---\nname: stub\n---\n");
}

#[test]
fn memory_dir_import_routes_backdates_redacts_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_corpus(root);

    let before = snapshot_bytes(root);

    let engine = MemoryEngine::builder(4).build().unwrap();
    let config = BootstrapConfig::default(); // redact = true

    // --- First pass ---
    let report = engine
        .bootstrap_memory_directory(root, &TestEmbedder, &config, None)
        .unwrap();

    assert_eq!(
        report.memory_files_parsed, 6,
        "6 files have a body (one stub skipped), got {}",
        report.memory_files_parsed
    );
    assert_eq!(
        report.memory_files_skipped, 1,
        "the frontmatter-only stub is skipped"
    );
    assert_eq!(report.facts_created, 6, "one fact per parsed file");
    assert_eq!(
        report.facts_reinforced, 0,
        "first pass creates, never reinforces"
    );
    assert!(
        report.secrets_redacted >= 1,
        "the planted AWS key must be counted as redacted, got {}",
        report.secrets_redacted
    );

    // --- Redaction: the secret is nowhere in the store ---
    let stored = engine.list_active_facts(None).unwrap();
    assert_eq!(stored.len(), 6);
    for f in &stored {
        assert!(
            !f.content.contains(PLANTED_SECRET),
            "planted secret leaked into stored fact: {:?}",
            f.content
        );
    }

    // --- Type routing ---
    let count_type = |t: FactType| stored.iter().filter(|f| f.fact_type == t).count();
    assert_eq!(count_type(FactType::Procedural), 1, "feedback → Procedural");
    assert_eq!(count_type(FactType::Episodic), 1, "project → Episodic");
    // user(2) + reference(1) + MEMORY-no-type(1) = 4 Semantic.
    assert_eq!(count_type(FactType::Semantic), 4);

    // reference fact carries the KB routing tag.
    let reference = stored
        .iter()
        .find(|f| f.metadata.get("memory_type").and_then(|v| v.as_str()) == Some("reference"))
        .expect("reference fact present");
    assert_eq!(
        reference.metadata.get("route").and_then(|v| v.as_str()),
        Some("knowledge-base"),
        "reference-typed memory tagged for KB relocation"
    );

    // --- Backdating: the project fact uses its frontmatter date (2024) ---
    let project = stored
        .iter()
        .find(|f| f.metadata.get("memory_type").and_then(|v| v.as_str()) == Some("project"))
        .expect("project fact present");
    assert_eq!(
        project.t_created.year(),
        2024,
        "project t_created backdated to valid_from 2024, got {}",
        project.t_created.year()
    );
    // No fact is stamped with a wall-clock (current-year) t_created via mtime:
    // the temp files' mtime IS the current year, so MEMORY.md (no frontmatter,
    // mtime-dated) legitimately carries the current year — assert only that the
    // explicitly-dated project fact is historical.
    assert!(project.t_created < chrono::Utc::now());

    // --- Idempotency: re-run creates nothing, reinforces everything ---
    let report2 = engine
        .bootstrap_memory_directory(root, &TestEmbedder, &config, None)
        .unwrap();
    assert_eq!(report2.facts_created, 0, "re-run must create 0 facts");
    assert_eq!(report2.facts_reinforced, 6, "re-run reinforces all 6");
    assert_eq!(report2.memory_files_parsed, 6);
    assert_eq!(
        report2.secrets_redacted, 0,
        "secrets_redacted is create-gated: a reinforced re-run re-scrubs but re-counts nothing"
    );
    assert_eq!(
        engine.list_active_facts(None).unwrap().len(),
        6,
        "store still holds exactly 6 facts after re-run"
    );

    // --- Sources read-only: every byte unchanged ---
    let after = snapshot_bytes(root);
    assert_eq!(
        before, after,
        "source .md files must be byte-identical after import"
    );
}

#[test]
fn filename_encoded_date_backdates_when_no_frontmatter_date() {
    // The real native corpus encodes the authored date in the filename and
    // carries no frontmatter date. Without filename parsing this would fall to
    // mtime (≈ current import time); assert it backdates to the filename's year.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(
        root,
        "project_decision_2024_05_01.md",
        "---\nname: decision\nmetadata:\n  type: project\n---\nWe chose option A.\n",
    );

    let engine = MemoryEngine::builder(4).build().unwrap();
    let config = BootstrapConfig::default();
    engine
        .bootstrap_memory_directory(root, &TestEmbedder, &config, None)
        .unwrap();

    let stored = engine.list_active_facts(None).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].t_created.year(),
        2024,
        "t_created must come from the filename date (2024), not mtime, got {}",
        stored[0].t_created.year()
    );
}

#[test]
fn frontmatter_secret_in_description_is_redacted() {
    // Regression (review BLOCKER): a secret in a frontmatter `description:` must
    // not reach the stored `metadata` column — redaction covers the whole row,
    // not just the body content.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(
        root,
        "incident.md",
        &format!(
            "---\nname: incident-note\ndescription: \"leaked key {PLANTED_SECRET}\"\nmetadata:\n  type: project\n---\nThe incident is resolved.\n"
        ),
    );

    let engine = MemoryEngine::builder(4).build().unwrap();
    let config = BootstrapConfig::default();
    let report = engine
        .bootstrap_memory_directory(root, &TestEmbedder, &config, None)
        .unwrap();

    assert_eq!(report.facts_created, 1);
    assert!(
        report.secrets_redacted >= 1,
        "the description secret must be counted as redacted, got {}",
        report.secrets_redacted
    );

    let stored = engine.list_active_facts(None).unwrap();
    assert_eq!(stored.len(), 1);
    let meta = &stored[0].metadata;
    assert!(
        !meta.to_string().contains(PLANTED_SECRET),
        "secret leaked into stored metadata: {meta}"
    );
    let desc = meta
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        desc.contains("[REDACTED:"),
        "description value should be scrubbed, got {desc:?}"
    );
}
