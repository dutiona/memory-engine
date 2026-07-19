//! #991: the documented gate matrix MUST be a superset of the cargo gate commands
//! `.github/workflows/ci.yml` runs.
//!
//! CI is now `workflow_dispatch`-only (#989), so the matrix in `CLAUDE.md` / `AGENTS.md` /
//! `GEMINI.md` *is* the verification, not a convenience mirror of CI. An incomplete matrix
//! is an unenforced gate — exactly the drift #990 found by hand (two `cargo check` gates CI
//! ran that the docs never listed). This guard runs inside `cargo test --workspace
//! --all-features` (itself a matrix line), so it can't be skipped by anyone who runs the
//! gate at all — unlike a CI-based checker, which is useless while CI is manual.
//!
//! The CI side is parsed as a YAML *subset* rather than line-scraped, because a guard with
//! blind spots is worse than none (it fails *open* — a drift slips through green). It walks
//! `jobs.<job>.steps[].run`, handling the real GitHub-Actions step forms: named
//! (`run: cargo …`), nameless (`- run: cargo …`), block scalars (`run: |`), and quoted
//! scalars; the `#`-comment strip is applied to BOTH sides so a valid trailing comment on a
//! CI line can't fail the guard *closed*. The `parser_*` mutation tests below lock each form.

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

/// Strip a trailing `# comment` and collapse the column-alignment padding the docs use
/// (`cargo test··--workspace`) so both sides compare equal.
fn norm(cmd: &str) -> String {
    cmd.split('#')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A `cargo` gate command — excludes `cargo install …` (toolchain setup, e.g. the fuzz
/// job's cargo-fuzz install), which is not a gate.
fn is_gate_cargo(cmd: &str) -> bool {
    cmd.starts_with("cargo ") && !cmd.starts_with("cargo install ")
}

/// Record `raw` as a gate command unless it belongs to the `msrv` job (deliberately
/// documented as prose, not in the matrix block — #991).
fn push_cargo(raw: &str, job: &str, out: &mut BTreeSet<String>) {
    if job == "msrv" {
        return;
    }
    let cmd = norm(raw.trim().trim_matches('"').trim_matches('\''));
    if is_gate_cargo(&cmd) {
        out.insert(cmd);
    }
}

/// Every `cargo` gate command CI actually runs, parsed as a YAML subset: walk each job's
/// steps, handling named / nameless / block-scalar / quoted `run:` forms, scoping the MSRV
/// exception to `jobs.msrv`.
fn ci_gate_commands(ci_yml: &str) -> BTreeSet<String> {
    let mut cmds = BTreeSet::new();
    let mut in_jobs = false;
    let mut job = String::new();
    let mut block_indent: Option<usize> = None; // indent of an open `run: |` block header
    for line in ci_yml.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();
        // Top-level key: only the `jobs:` map is relevant.
        if indent == 0 {
            in_jobs = trimmed == "jobs:";
            job.clear();
            block_indent = None;
            continue;
        }
        if !in_jobs {
            continue;
        }
        // Job header: `  <name>:` at 2-space indent (nothing else sits at that depth).
        if indent == 2 && trimmed.ends_with(':') && !trimmed.contains(char::is_whitespace) {
            job = trimmed.trim_end_matches(':').to_string();
            block_indent = None;
            continue;
        }
        // Body of an open `run: |` block scalar: deeper-indented lines are commands.
        if let Some(bi) = block_indent {
            if indent > bi {
                push_cargo(trimmed, &job, &mut cmds);
                continue;
            }
            block_indent = None; // dedent closes the block
        }
        // A `run:` step, named (`run: …`) or nameless (`- run: …`).
        if let Some(body) = trimmed.trim_start_matches("- ").strip_prefix("run:") {
            let body = body.trim();
            if matches!(body, "|" | "|-" | ">" | ">-") {
                block_indent = Some(indent);
            } else {
                push_cargo(body, &job, &mut cmds);
            }
        }
    }
    cmds
}

/// The `cargo` lines from a doc's gate-matrix fenced block. Fenced code blocks are the
/// odd-indexed chunks of a ```` ``` ````-split; select the single one carrying the
/// `cargo fmt --all --check` anchor (require exactly one, so a prose mention or a second
/// example block can't silently substitute for the canonical gate block).
fn doc_matrix_commands(doc: &str, doc_name: &str) -> BTreeSet<String> {
    let gate_blocks: Vec<&str> = doc
        .split("```")
        .skip(1)
        .step_by(2)
        .filter(|b| b.contains("cargo fmt --all --check"))
        .collect();
    assert_eq!(
        gate_blocks.len(),
        1,
        "{doc_name}: expected exactly ONE fenced code block carrying the \
         `cargo fmt --all --check` gate anchor, found {}",
        gate_blocks.len()
    );
    gate_blocks[0]
        .lines()
        .map(norm)
        .filter(|c| c.starts_with("cargo "))
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

// --- parser mutation tests: each locks a `run:` form the guard must NOT miss (fail-open)
//     or wrongly reject (fail-closed). Regressions here were real review findings on #996. ---

#[test]
fn parser_catches_nameless_single_line_run() {
    let yml = "jobs:\n  x:\n    steps:\n      - run: cargo audit --workspace\n";
    assert!(ci_gate_commands(yml).contains("cargo audit --workspace"));
}

#[test]
fn parser_catches_block_scalar_run() {
    let yml =
        "jobs:\n  x:\n    steps:\n      - run: |\n          cargo test --workspace --secret-flag\n";
    assert!(ci_gate_commands(yml).contains("cargo test --workspace --secret-flag"));
}

#[test]
fn parser_catches_quoted_run() {
    let yml = "jobs:\n  x:\n    steps:\n      - run: \"cargo deny check\"\n";
    assert!(ci_gate_commands(yml).contains("cargo deny check"));
}

#[test]
fn parser_scopes_msrv_exclusion_to_the_job() {
    let msrv =
        "jobs:\n  msrv:\n    steps:\n      - run: cargo build --workspace --tests --examples\n";
    assert!(
        ci_gate_commands(msrv).is_empty(),
        "the msrv job's gates are excluded"
    );
    let build =
        "jobs:\n  build:\n    steps:\n      - run: cargo build --workspace --tests --examples\n";
    assert!(
        ci_gate_commands(build).contains("cargo build --workspace --tests --examples"),
        "an unrelated --tests --examples gate is NOT excluded"
    );
}

#[test]
fn parser_strips_trailing_comments_symmetrically() {
    let yml = "jobs:\n  x:\n    steps:\n      - run: cargo build --workspace # keep this\n";
    assert!(
        ci_gate_commands(yml).contains("cargo build --workspace"),
        "a trailing # comment on a CI run must not fail the guard closed"
    );
}

#[test]
fn parser_excludes_cargo_install_setup() {
    let yml = "jobs:\n  x:\n    steps:\n      - run: cargo install cargo-fuzz --locked\n";
    assert!(ci_gate_commands(yml).is_empty());
}
