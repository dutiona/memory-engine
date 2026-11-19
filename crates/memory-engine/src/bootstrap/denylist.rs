//! The author-seeded secret denylist (#51): bootstrap's input to the redaction engine.

/// Environment variable naming a file of author-known secret literals, one per line.
///
/// Only the *path* enters the environment; the secret values never do — they live in
/// the file alone. That file MUST be kept out of version control (caller's
/// responsibility, as the path may point anywhere); `.secrets-denylist` at the repo root
/// is pre-listed in `.gitignore` as a safe default.
pub const DENYLIST_ENV_VAR: &str = "ME_REDACT_DENYLIST_FILE";

/// Parse denylist file contents into literals: one secret per line, trimmed,
/// skipping blank lines and `#`-comments. Pure (no I/O) so it is unit-tested
/// without a temp file.
fn parse_denylist(contents: &str) -> Vec<String> {
    contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// Load the author-seeded secret denylist from the file named by [`DENYLIST_ENV_VAR`].
///
/// Returns an empty list (→ signatures-only mode) when the variable is unset. The
/// referenced file is the only place real secrets live and must be kept out of version
/// control (caller's responsibility; `.secrets-denylist` is a pre-`.gitignore`d default).
/// Feed the result to the bootstrap pipeline's `BootstrapConfig::denylist`.
///
/// Note: an unset variable degrading to signatures-only is silent by design here; the #53
/// backfill caller should log how many literals loaded so "denylist active" vs "silently
/// empty" is never inferred from a missing var.
///
/// # Errors
/// Returns an error only when the variable is set but its file cannot be read.
pub fn load_secret_denylist() -> std::io::Result<Vec<String>> {
    let Some(path) = std::env::var_os(DENYLIST_ENV_VAR) else {
        return Ok(Vec::new());
    };
    // Read directly and add path context on failure rather than pre-checking
    // `is_file()`: the read already errors on a missing path or directory, and a
    // check-then-read pre-flight is a TOCTOU race for no gain. The preserved
    // `e.kind()` still distinguishes NotFound vs IsADirectory for callers.
    let contents = std::fs::read_to_string(&path).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!(
                "reading denylist file {} (from {DENYLIST_ENV_VAR}): {e}",
                std::path::Path::new(&path).display()
            ),
        )
    })?;
    Ok(parse_denylist(&contents))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_denylist_skips_blanks_and_comments() {
        let contents = "\
# my known secrets — gitignored, never committed
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855

  hunter2-dev-pw
# trailing comment
";
        let parsed = parse_denylist(contents);
        assert_eq!(
            parsed,
            vec![
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_owned(),
                "hunter2-dev-pw".to_owned(),
            ]
        );
    }
}
