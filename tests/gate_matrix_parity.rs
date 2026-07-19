//! #991: the documented gate matrix MUST be a superset of the cargo gate commands
//! `.github/workflows/ci.yml` runs.
//!
//! CI is now `workflow_dispatch`-only (#989), so the matrix in `CLAUDE.md` / `AGENTS.md` /
//! `GEMINI.md` *is* the verification, not a convenience mirror of CI. An incomplete matrix
//! is an unenforced gate — exactly the drift #990 found by hand (two `cargo check` gates CI
//! ran that the docs never listed). This guard runs inside `cargo test --workspace
//! --all-features` (itself a matrix line), so it cannot be skipped by anyone who runs the
//! gate at all — unlike a CI-based checker, which is useless while CI is manual.
//!
//! It fails the moment someone adds a `run: cargo …` gate to `ci.yml` without documenting
//! it in all three matrices (docs may *add* commands — e.g. `cargo deny check`, which CI
//! runs via an action rather than `run:` — but never omit one CI runs).

use std::collections::BTreeSet;
use std::path::PathBuf;

/// This test's crate manifest is at `<repo>/memory-engine/lib/memory-engine`; the docs and
/// `ci.yml` it checks live at the repo root, three levels up.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves from CARGO_MANIFEST_DIR")
}

/// Collapse the column-alignment padding the docs use (`cargo test··--workspace`) so it
/// compares equal to CI's single-spaced form.
fn norm(cmd: &str) -> String {
    cmd.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every `cargo` gate command CI actually runs, minus two documented exceptions:
/// - `cargo install …` — toolchain setup, not a gate (the fuzz job installs cargo-fuzz).
/// - `… --tests --examples` — the MSRV job, deliberately documented as prose, not in the
///   matrix block (#991).
fn ci_gate_commands(ci_yml: &str) -> BTreeSet<String> {
    ci_yml
        .lines()
        .filter_map(|l| l.trim().strip_prefix("run: "))
        .map(str::trim)
        .filter(|c| c.starts_with("cargo "))
        .filter(|c| !c.starts_with("cargo install "))
        .filter(|c| !c.contains("--tests --examples"))
        .map(norm)
        .collect()
}

/// The `cargo` lines from a doc's gate-matrix fenced block (the one carrying the
/// `cargo fmt --all --check` anchor), with trailing `# comments` and padding stripped.
fn doc_matrix_commands(doc: &str, doc_name: &str) -> BTreeSet<String> {
    let block = doc
        .split("```")
        .find(|b| b.contains("cargo fmt --all --check"))
        .unwrap_or_else(|| {
            panic!("{doc_name}: no gate-matrix bash block (anchor `cargo fmt --all --check`) found")
        });
    block
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|c| c.starts_with("cargo "))
        .map(norm)
        .collect()
}

#[test]
fn docs_gate_matrix_is_a_superset_of_ci() {
    let root = repo_root();
    let ci = std::fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read ci.yml");
    let ci_cmds = ci_gate_commands(&ci);
    assert!(
        !ci_cmds.is_empty(),
        "parsed zero cargo gate commands from ci.yml — the parser broke, not the docs"
    );

    for doc_name in ["CLAUDE.md", "AGENTS.md", "GEMINI.md"] {
        let doc = std::fs::read_to_string(root.join(doc_name))
            .unwrap_or_else(|e| panic!("read {doc_name}: {e}"));
        let doc_cmds = doc_matrix_commands(&doc, doc_name);
        let missing: Vec<&String> = ci_cmds.difference(&doc_cmds).collect();
        assert!(
            missing.is_empty(),
            "{doc_name}'s gate matrix omits cargo commands that ci.yml runs \
             (the docs matrix must be a SUPERSET of CI — #991):\n  missing: {missing:#?}\n\n  \
             ci gate commands: {ci_cmds:#?}\n  doc matrix commands: {doc_cmds:#?}"
        );
    }
}
