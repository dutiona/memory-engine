//! Secrets/PII redaction gate (#45 S0.5).
//!
//! Sits in **front** of any bulk import of session `.jsonl` into ME/KB: 9 months of
//! dev sessions contain credentials, tokens and personal data. This is a HARD
//! prerequisite for the B3 backfill (#53) and is reviewed by the owner (#51) before
//! any backfill runs — so it is **build-only** here and is *not* wired into
//! `bootstrap_session` yet.
//!
//! Detection is dependency-free and uses a **multi-detector span scanner**: each
//! detector finds redaction spans over the raw text, non-overlapping spans are
//! collected in priority order (earlier detectors win), and the string is rebuilt
//! replacing each span with `[REDACTED:<rule>]`. Char indices are used throughout
//! to avoid panics on multibyte text.
//!
//! Detectors (in priority order):
//! 1. **provider-prefix** — known secret-token prefixes at a token boundary.
//! 2. **url-credential** — `user:password@host` inside a URL authority.
//! 3. **email** — `local@domain.tld` PII.
//! 4. **hex-in-context** — long hex run preceded by a secret-indicator key.
//! 5. **high-entropy** — arbitrary long tokens with high Shannon entropy.

use std::collections::{BTreeMap, HashMap};

// ── Constants ────────────────────────────────────────────────────────────────

/// Known secret-token prefixes: (rule name, literal prefix). gitleaks-style.
const SECRET_PREFIXES: &[(&str, &str)] = &[
    ("aws-access-key", "AKIA"),
    ("github-pat", "ghp_"),
    ("github-fine-pat", "github_pat_"),
    ("openai-key", "sk-"),
    ("google-api-key", "AIza"),
    ("slack-token", "xoxb-"),
    ("slack-token", "xoxp-"),
    ("private-key-header", "-----BEGIN"),
    ("jwt", "eyJ"), // base64url of `{"` — catches JWTs
    ("sendgrid", "SG."),
];

/// Characters that constitute a "structural boundary" for the provider-prefix
/// detector: a prefix is only a match if it is at the text start, or preceded
/// by whitespace or one of these structural chars.
const PREFIX_BOUNDARY_CHARS: &[char] = &[
    '"', '\'', '`', '(', ')', '{', '}', '[', ']', ':', ',', ';', '=', '<', '>', '?', '&', '/', '@',
];

/// Characters that are valid inside a provider-token body (extends into the
/// secret after the prefix).
const TOKEN_BODY_CHARS: &[char] = &['.', '_', '-', '+', '/', '='];

/// Characters on which the high-entropy detector splits tokens.
/// Intentionally does NOT include `.` or `/` (those appear inside secrets).
const ENTROPY_SPLIT_CHARS: &[char] = &[
    '"', '\'', '`', '{', '}', '[', ']', '(', ')', ':', ',', ';', '=',
];

/// Secret-indicator keywords that gate the hex-in-context detector (case-insensitive).
const HEX_CONTEXT_KEYS: &[&str] = &[
    // Base identifier components: a trailing key like `api_key` / `access_token` gates the
    // hex via its `key` / `token` component (the word is split on `_`), but ordinary words
    // such as `monkey` / `authentic` do not.
    "secret",
    "token",
    "key",
    "apikey",
    "password",
    "passwd",
    "pwd",
    "auth",
    "credential",
    "cred",
];

/// Minimum hex run length to be considered a secret (when context-gated).
const HEX_SECRET_MIN_LEN: usize = 32;

/// Minimum length of an author-supplied denylist literal to be honored. Guards
/// against a stray short/blank line in the (gitignored) denylist source matching
/// everywhere and nuking the corpus. The author's real secrets are far longer.
const DENYLIST_MIN_LEN: usize = 4;

/// Minimum length of a prefix-detected token (prefix + body). Rejects bare/short matches
/// that fire on ordinary words (e.g. `SG.`, `eyJournal`); real provider tokens are long
/// (AWS `AKIA` + 16 = 20 is the shortest).
const MIN_PREFIXED_SECRET_LEN: usize = 20;

/// Minimum token length for the high-entropy detector.
const HIGH_ENTROPY_MIN_LEN: usize = 20;

/// Shannon entropy threshold (bits/char) for the high-entropy detector.
const HIGH_ENTROPY_BITS: f64 = 4.3;

// ── Public types ─────────────────────────────────────────────────────────────

/// One redaction: the rule that fired and the (now-removed) secret text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: String,
    pub token: String,
}

/// Auditable tally of what was redacted — the report the owner reviews in #51.
#[derive(Debug, Clone, Default)]
pub struct RedactionReport {
    pub by_rule: BTreeMap<String, usize>,
    pub total: usize,
    pub entries_scanned: usize,
}

impl RedactionReport {
    fn record(&mut self, rule: &str) {
        *self.by_rule.entry(rule.to_owned()).or_default() += 1;
        self.total += 1;
    }

    /// Human-readable coverage lines for the audit report.
    #[must_use]
    pub fn coverage_lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "scanned {} entries, redacted {} secrets/PII",
            self.entries_scanned, self.total
        )];
        lines.extend(
            self.by_rule
                .iter()
                .map(|(rule, n)| format!("  {rule}: {n}")),
        );
        lines
    }
}

// ── Shannon entropy (public — used by tests) ─────────────────────────────────

/// Shannon entropy of `s` in bits per character.
#[must_use]
pub fn shannon_entropy(s: &str) -> f64 {
    let n = s.chars().count();
    if n == 0 {
        return 0.0;
    }
    let mut counts: HashMap<char, usize> = HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_default() += 1;
    }
    #[allow(clippy::cast_precision_loss)]
    let n_f = n as f64;
    #[allow(clippy::cast_precision_loss)]
    counts
        .values()
        .map(|&c| {
            let p = c as f64 / n_f;
            -p * p.log2()
        })
        .sum()
}

// ── Span type ────────────────────────────────────────────────────────────────

/// A half-open byte range `[start, end)` in the original text together with the
/// rule name.  All indices are **byte** offsets into the UTF-8 string (produced
/// from char positions), and are always kept on char boundaries.
#[derive(Debug)]
struct Span {
    start: usize, // byte offset
    end: usize,   // byte offset (exclusive)
    rule: &'static str,
}

/// Returns true if `[a_start, a_end)` overlaps with any span in `spans`.
fn overlaps(spans: &[Span], a_start: usize, a_end: usize) -> bool {
    spans.iter().any(|s| a_start < s.end && a_end > s.start)
}

// ── Detector 0: author-seeded denylist (highest priority) ─────────────────────

/// Redact every occurrence of each author-supplied known-secret literal.
///
/// This closes the residual gaps of the heuristic detectors for a **controlled
/// single-author corpus**: bare hex (information-theoretically indistinguishable
/// from a SHA/checksum) and arbitrary low-entropy tokens carry no prefix, context
/// key, or entropy signal, so no signature can catch them without over-redacting
/// non-secret data. But when the secret-holder is *known*, their secrets can be
/// *enumerated* and matched literally with near-zero false positives.
///
/// Runs first so an author-known value wins priority over any signature rule.
/// Literals shorter than [`DENYLIST_MIN_LEN`] **bytes** are skipped (over-redaction
/// guard; byte length so a short-char/multibyte secret the author listed is honored).
/// The denylist values themselves are supplied at runtime from a gitignored source
/// and are never compiled into this crate.
///
/// Matching is authoritative *within* this detector: every occurrence of every
/// literal is collected and overlapping/extending ranges are **merged** into maximal
/// spans before emission, so an author who enumerates overlapping secrets (a token and
/// a longer line containing it; two secrets sharing an interior region) gets full
/// coverage regardless of iteration order — no partial-overlap tail leak. Matches that
/// fall inside an existing `[REDACTED:...]` placeholder are skipped so re-running on
/// already-redacted text is idempotent even when a literal collides with placeholder text.
fn detect_denylist(text: &str, denylist: &[String], spans: &mut Vec<Span>) {
    let placeholders = placeholder_spans(text);

    // 1. Collect every match range for every literal (≥ DENYLIST_MIN_LEN bytes),
    //    skipping any match inside an existing placeholder.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for literal in denylist {
        if literal.len() < DENYLIST_MIN_LEN {
            continue;
        }
        let mut search_start = 0;
        while let Some(rel) = text[search_start..].find(literal.as_str()) {
            let start = search_start + rel;
            let end = start + literal.len();
            if !range_overlaps(&placeholders, start, end) {
                ranges.push((start, end));
            }
            // Advance past this match; guard against a zero-width step.
            search_start = end.max(start + 1);
        }
    }
    if ranges.is_empty() {
        return;
    }

    // 2. Merge truly-overlapping ranges into maximal spans (sorted by start; merge
    //    when the next range starts strictly before the current end — adjacent,
    //    non-overlapping ranges stay distinct).
    ranges.sort_unstable();
    let mut cur = ranges[0];
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for &(s, e) in &ranges[1..] {
        if s < cur.1 {
            cur.1 = cur.1.max(e);
        } else {
            merged.push(cur);
            cur = (s, e);
        }
    }
    merged.push(cur);

    // 3. Emit merged spans (denylist runs first, so `spans` is normally empty here;
    //    the overlap guard keeps the non-overlap invariant if ordering ever changes).
    for (start, end) in merged {
        if !overlaps(spans, start, end) {
            spans.push(Span {
                start,
                end,
                rule: "denylist",
            });
        }
    }
}

/// Byte ranges of existing `[REDACTED:<rule>]` placeholders, so the denylist detector
/// never re-redacts inside a prior pass's output (idempotency). The other detectors are
/// naturally placeholder-safe; only literal denylist matching can collide with one.
fn placeholder_spans(text: &str) -> Vec<(usize, usize)> {
    const OPEN: &str = "[REDACTED:";
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = text[from..].find(OPEN) {
        let start = from + rel;
        let Some(close_rel) = text[start..].find(']') else {
            break;
        };
        let end = start + close_rel + 1;
        out.push((start, end));
        from = end;
    }
    out
}

/// Returns true if `[start, end)` overlaps any `(s, e)` range in `ranges`.
fn range_overlaps(ranges: &[(usize, usize)], start: usize, end: usize) -> bool {
    ranges.iter().any(|&(s, e)| start < e && end > s)
}

// ── Detector 1: provider-prefix ──────────────────────────────────────────────

fn is_prefix_boundary(text: &str, byte_pos: usize) -> bool {
    if byte_pos == 0 {
        return true;
    }
    // Walk backwards one char
    let preceding = &text[..byte_pos];
    if let Some(ch) = preceding.chars().next_back() {
        return ch.is_whitespace() || PREFIX_BOUNDARY_CHARS.contains(&ch);
    }
    true
}

fn detect_provider_prefix(text: &str, spans: &mut Vec<Span>) {
    for &(rule, prefix) in SECRET_PREFIXES {
        let mut search_start = 0;
        while search_start < text.len() {
            let Some(rel) = text[search_start..].find(prefix) else {
                break;
            };
            let match_start = search_start + rel;

            // Boundary check
            if !is_prefix_boundary(text, match_start) {
                search_start = match_start + 1;
                continue;
            }

            // Special case: -----BEGIN — redact to end of line
            let match_end = if prefix == "-----BEGIN" {
                let rest = &text[match_start..];
                let mut line_end = rest.find('\n').unwrap_or(rest.len());
                // Keep \r\n as the line terminator — don't swallow a trailing CR.
                if line_end > 0 && rest.as_bytes()[line_end - 1] == b'\r' {
                    line_end -= 1;
                }
                match_start + line_end
            } else {
                // Extend over the token body: alnum + TOKEN_BODY_CHARS
                let body_start = match_start + prefix.len();
                let mut end = body_start;
                for ch in text[body_start..].chars() {
                    if ch.is_alphanumeric() || TOKEN_BODY_CHARS.contains(&ch) {
                        end += ch.len_utf8();
                    } else {
                        break;
                    }
                }
                end
            };

            // Reject bare/short matches that fire on ordinary words (`SG.`, `eyJournal`):
            // real provider tokens are long, and the dot-structured prefixes (`SG.`, `eyJ`)
            // must actually exhibit their dotted multi-segment structure.
            if prefix != "-----BEGIN" {
                let captured = &text[match_start..match_end];
                if captured.len() < MIN_PREFIXED_SECRET_LEN
                    || (matches!(prefix, "SG." | "eyJ") && !captured.contains('.'))
                {
                    search_start = match_start + 1;
                    continue;
                }
            }

            if !overlaps(spans, match_start, match_end) {
                spans.push(Span {
                    start: match_start,
                    end: match_end,
                    rule,
                });
            }
            search_start = match_start + 1;
        }
    }
}

// ── Detector 2: url-credential ───────────────────────────────────────────────

fn detect_url_credential(text: &str, spans: &mut Vec<Span>) {
    let mut search_start = 0;
    while search_start < text.len() {
        let Some(rel) = text[search_start..].find("://") else {
            break;
        };
        let authority_start = search_start + rel + 3; // after "://"

        // Authority ends at the next '/', '?', '#', whitespace, or end
        let authority_text = &text[authority_start..];
        let authority_len = authority_text
            .find(|c: char| c == '/' || c == '?' || c == '#' || c.is_whitespace())
            .unwrap_or(authority_text.len());
        let authority = &authority_text[..authority_len];

        // Need 'user:pass@host' — must contain '@' AND first ':' before last '@'
        if let Some(at_pos) = authority.rfind('@') {
            let before_at = &authority[..at_pos];
            if let Some(colon_pos) = before_at.find(':') {
                // password = between first ':' and last '@'
                let pass_start = authority_start + colon_pos + 1;
                let pass_end = authority_start + at_pos;
                if pass_start < pass_end && !overlaps(spans, pass_start, pass_end) {
                    spans.push(Span {
                        start: pass_start,
                        end: pass_end,
                        rule: "connection-string-password",
                    });
                }
            }
        }

        search_start = authority_start + authority_len + 1;
    }
}

// ── Detector 3: email ────────────────────────────────────────────────────────

/// True if `ch` is valid in an email local-part.
fn is_email_local(ch: char) -> bool {
    ch.is_alphanumeric() || "._%+-".contains(ch)
}

/// True if `ch` is valid in an email domain.
fn is_email_domain(ch: char) -> bool {
    ch.is_alphanumeric() || ".-".contains(ch)
}

fn detect_email(text: &str, spans: &mut Vec<Span>) {
    // Find each '@' and expand left (local-part) and right (domain).
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (at_idx, &(at_byte, ch)) in chars.iter().enumerate() {
        if ch != '@' {
            continue;
        }

        // Expand left for local-part
        let mut local_start_idx = at_idx;
        while local_start_idx > 0 && is_email_local(chars[local_start_idx - 1].1) {
            local_start_idx -= 1;
        }
        let local = &text[chars[local_start_idx].0..at_byte];
        if local.is_empty() {
            continue;
        }

        // Expand right for domain
        let domain_start_idx = at_idx + 1;
        let mut domain_end_idx = domain_start_idx;
        while domain_end_idx < chars.len() && is_email_domain(chars[domain_end_idx].1) {
            domain_end_idx += 1;
        }
        let domain_end_byte = if domain_end_idx < chars.len() {
            chars[domain_end_idx].0
        } else {
            text.len()
        };
        let domain = &text[at_byte + 1..domain_end_byte];

        // Validate: domain must contain at least one '.' and end in 2+ alpha TLD
        if !domain.contains('.') {
            continue;
        }
        let tld = domain.rsplit('.').next().unwrap_or("");
        if tld.len() < 2 || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
            continue;
        }

        let span_start = chars[local_start_idx].0;
        let span_end = domain_end_byte;

        if !overlaps(spans, span_start, span_end) {
            spans.push(Span {
                start: span_start,
                end: span_end,
                rule: "email",
            });
        }
    }
}

// ── Detector 4: hex-in-context ───────────────────────────────────────────────

/// True if `ch` is a hex digit.
const fn is_hex(ch: char) -> bool {
    ch.is_ascii_hexdigit()
}

/// True iff the WORD immediately preceding the hex run is a secret-indicator key.
///
/// Word-adjacency (separated only by assignment punctuation / quotes / whitespace), NOT a
/// line-wide substring scan: so `monkey <hex>`, `authentic <hex>`, and
/// `key.rs: ... points to <hex>` do NOT gate (the bare hex / git SHA is preserved), while
/// `api_key=<hex>`, `secret: <hex>`, and `"token":"<hex>"` do. The trailing word is split
/// on `_`, so `api_key` / `access_token` gate via their `key` / `token` component.
fn has_hex_context_key(line: &str, hex_start_in_line: usize) -> bool {
    let prefix = &line[..hex_start_in_line];
    let trimmed = prefix.trim_end_matches(|c: char| c.is_whitespace() || "=:\"'`".contains(c));
    let word_len: usize = trimmed
        .chars()
        .rev()
        .take_while(|&c| c.is_alphanumeric() || c == '_')
        .map(char::len_utf8)
        .sum();
    if word_len == 0 {
        return false;
    }
    let word = trimmed[trimmed.len() - word_len..].to_ascii_lowercase();
    word.split('_').any(|part| HEX_CONTEXT_KEYS.contains(&part))
}

fn detect_hex_in_context(text: &str, spans: &mut Vec<Span>) {
    // Process line by line so we can check context within the same line.
    let mut line_start_byte = 0;
    for line in text.split('\n') {
        let chars: Vec<(usize, char)> = line.char_indices().collect();
        let mut i = 0;
        while i < chars.len() {
            if !is_hex(chars[i].1) {
                i += 1;
                continue;
            }
            // Find end of hex run
            let hex_start_local = chars[i].0; // byte offset within line
            let mut j = i;
            while j < chars.len() && is_hex(chars[j].1) {
                j += 1;
            }
            let hex_end_local = if j < chars.len() {
                chars[j].0
            } else {
                line.len()
            };
            let run_len = j - i; // number of hex chars

            if run_len >= HEX_SECRET_MIN_LEN && has_hex_context_key(line, hex_start_local) {
                let span_start = line_start_byte + hex_start_local;
                let span_end = line_start_byte + hex_end_local;
                if !overlaps(spans, span_start, span_end) {
                    spans.push(Span {
                        start: span_start,
                        end: span_end,
                        rule: "hex-secret",
                    });
                }
            }
            i = j;
        }
        line_start_byte += line.len() + 1; // +1 for the '\n'
    }
}

// ── Detector 5: high-entropy ─────────────────────────────────────────────────

fn is_entropy_split(ch: char) -> bool {
    ch.is_whitespace() || ENTROPY_SPLIT_CHARS.contains(&ch)
}

fn looks_like_secret_charset(token: &str) -> bool {
    let charset_ok = token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "+/=_-".contains(c));
    let has_digit = token.chars().any(|c| c.is_ascii_digit());
    let has_special = token.chars().any(|c| "+/=_-".contains(c));
    charset_ok && (has_digit || has_special)
}

fn detect_high_entropy(text: &str, spans: &mut Vec<Span>) {
    // Walk byte-by-byte splitting on entropy-split chars, collecting token ranges.
    let mut tok_start: Option<usize> = None;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let flush = |tok_start: usize, tok_end: usize, spans: &mut Vec<Span>| {
        let token = &text[tok_start..tok_end];
        if token.chars().count() >= HIGH_ENTROPY_MIN_LEN
            && looks_like_secret_charset(token)
            && shannon_entropy(token) >= HIGH_ENTROPY_BITS
            && !overlaps(spans, tok_start, tok_end)
        {
            spans.push(Span {
                start: tok_start,
                end: tok_end,
                rule: "high-entropy",
            });
        }
    };

    for &(byte_pos, ch) in &chars {
        if is_entropy_split(ch) {
            if let Some(start) = tok_start.take() {
                flush(start, byte_pos, spans);
            }
        } else if tok_start.is_none() {
            tok_start = Some(byte_pos);
        }
    }
    if let Some(start) = tok_start {
        flush(start, text.len(), spans);
    }
}

// ── Span collection + string reconstruction ───────────────────────────────────

/// Run all detectors in priority order; collect non-overlapping spans.
///
/// `denylist` holds author-supplied known-secret literals (empty for the
/// signature-only path). Detector 0 (denylist) runs first so author-known
/// values win priority over the heuristic rules.
fn collect_spans(text: &str, denylist: &[String]) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    detect_denylist(text, denylist, &mut spans);
    detect_provider_prefix(text, &mut spans);
    detect_url_credential(text, &mut spans);
    detect_email(text, &mut spans);
    detect_hex_in_context(text, &mut spans);
    detect_high_entropy(text, &mut spans);
    // Sort by start position for reconstruction.
    spans.sort_by_key(|s| s.start);
    spans
}

/// Rebuild `text` replacing each span with `[REDACTED:<rule>]`.
///
/// Spans must be sorted and non-overlapping (invariant guaranteed by
/// `collect_spans`).
fn rebuild(text: &str, spans: &[Span]) -> (String, Vec<Finding>) {
    let mut out = String::with_capacity(text.len());
    let mut findings = Vec::new();
    let mut cursor = 0usize;
    for span in spans {
        // Preserve everything before this span
        out.push_str(&text[cursor..span.start]);
        // Emit placeholder
        out.push_str("[REDACTED:");
        out.push_str(span.rule);
        out.push(']');
        findings.push(Finding {
            rule: span.rule.to_owned(),
            token: text[span.start..span.end].to_owned(),
        });
        cursor = span.end;
    }
    out.push_str(&text[cursor..]);
    (out, findings)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Redact secrets/PII from `text`, preserving all non-secret characters
/// byte-exact.
///
/// Returns the redacted text and the list of findings (each secret that was
/// replaced).  Redaction is idempotent: a `[REDACTED:*]` placeholder is never
/// re-redacted.
#[must_use]
pub fn redact_text(text: &str) -> (String, Vec<Finding>) {
    redact_text_with_denylist(text, &[])
}

/// Redact secrets/PII from `text`, additionally redacting every author-supplied
/// known-secret literal in `denylist` (priority over the signature rules).
///
/// Use this on a controlled corpus where the secret-holder can enumerate their
/// own secrets: it closes the bare-hex / arbitrary-low-entropy gaps the
/// signature detectors cannot (see [`detect_denylist`]). The `denylist` values
/// are supplied at runtime from a gitignored source — never committed.
///
/// `redact_text(text)` == `redact_text_with_denylist(text, &[])`.
#[must_use]
pub fn redact_text_with_denylist(text: &str, denylist: &[String]) -> (String, Vec<Finding>) {
    // Idempotency: text that only contains placeholders is a no-op.
    // The detectors naturally avoid re-redacting because:
    //  - `[REDACTED:...]` does not match any prefix, email, hex, or entropy rule.
    // No special guard needed; the invariant holds by construction.
    let spans = collect_spans(text, denylist);
    rebuild(text, &spans)
}

/// Redact a sequence of session entries (raw `.jsonl` lines or extracted text),
/// returning the redacted entries and an auditable coverage report.
#[must_use]
pub fn redact_entries(entries: &[String]) -> (Vec<String>, RedactionReport) {
    redact_entries_with_denylist(entries, &[])
}

/// Redact session entries, also redacting each author-supplied denylist literal.
///
/// This is the wired backfill path: callers load `denylist` from a gitignored
/// source and pass it here in front of any bulk import (see `bootstrap`).
/// `redact_entries(entries)` == `redact_entries_with_denylist(entries, &[])`.
#[must_use]
pub fn redact_entries_with_denylist(
    entries: &[String],
    denylist: &[String],
) -> (Vec<String>, RedactionReport) {
    let mut report = RedactionReport::default();
    let mut redacted = Vec::with_capacity(entries.len());
    for entry in entries {
        let (clean, findings) = redact_text_with_denylist(entry, denylist);
        report.entries_scanned += 1;
        for finding in &findings {
            report.record(&finding.rule);
        }
        redacted.push(clean);
    }
    (redacted, report)
}

/// Redact every string leaf of a JSON value in place, returning the total
/// finding count.
///
/// Walks objects (values), arrays (elements), and a bare string root; object
/// **keys are left intact** (they are structural identifiers, not content).
/// Used to scrub fact `metadata` before it is persisted — frontmatter
/// `name`/`description` on the `.md` path and any pluggable extractor's
/// `metadata` on the `.jsonl` path — so the redaction gate covers the whole
/// stored row, not just the fact content. Idempotent (a `[REDACTED:*]`
/// placeholder is never re-matched).
pub fn redact_json_strings(value: &mut serde_json::Value, denylist: &[String]) -> usize {
    match value {
        serde_json::Value::String(s) => {
            let (clean, findings) = redact_text_with_denylist(s, denylist);
            if !findings.is_empty() {
                *s = clean;
            }
            findings.len()
        }
        serde_json::Value::Array(items) => items
            .iter_mut()
            .map(|v| redact_json_strings(v, denylist))
            .sum(),
        serde_json::Value::Object(map) => map
            .values_mut()
            .map(|v| redact_json_strings(v, denylist))
            .sum(),
        // Numbers, booleans, and null carry no redactable text.
        _ => 0,
    }
}

// ── Author-seeded denylist loading (#51) ──────────────────────────────────────

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
/// Feed the result to [`redact_entries_with_denylist`].
///
/// Note: an unset variable degrading to signatures-only is silent by design here (the
/// gate is not yet wired); the #53 backfill caller should log how many literals loaded
/// so "denylist active" vs "silently empty" is never inferred from a missing var.
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
                "reading denylist file {:?} (from {DENYLIST_ENV_VAR}): {e}",
                std::path::Path::new(&path)
            ),
        )
    })?;
    Ok(parse_denylist(&contents))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── JSON metadata redaction ───────────────────────────────────────────────

    #[test]
    fn redact_json_strings_scrubs_nested_values_keeps_keys() {
        let mut v = serde_json::json!({
            "description": "leaked AKIAIOSFODNN7EXAMPLE here",
            "nested": { "note": "token ghp_0123456789abcdefghij0123456789abcdef" },
            "list": ["sk-abcdefghijklmnopqrstuvwxyz0123", "harmless"],
            "count": 7,
            "flag": true,
        });
        let n = redact_json_strings(&mut v, &[]);
        assert_eq!(n, 3, "three secrets across the tree");
        let s = v.to_string();
        assert!(!s.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!s.contains("ghp_0123456789abcdefghij0123456789abcdef"));
        assert!(!s.contains("sk-abcdefghijklmnopqrstuvwxyz0123"));
        // Structural keys + non-secret leaves survive intact.
        assert!(v.get("description").is_some());
        assert_eq!(v["list"][1], "harmless");
        assert_eq!(v["count"], 7);
        assert_eq!(v["flag"], true);
    }

    #[test]
    fn redact_json_strings_clean_input_is_noop() {
        let mut v = serde_json::json!({ "a": "nothing secret", "b": [1, 2, 3] });
        let before = v.clone();
        assert_eq!(redact_json_strings(&mut v, &[]), 0);
        assert_eq!(v, before);
    }

    // ── Provider prefixes ─────────────────────────────────────────────────────

    #[test]
    fn redacts_provider_prefixes() {
        for (token, rule) in [
            ("AKIAIOSFODNN7EXAMPLE", "aws-access-key"),
            ("ghp_0123456789abcdefghij0123456789abcdef", "github-pat"),
            ("sk-abcdefghijklmnopqrstuvwxyz0123", "openai-key"),
        ] {
            let (out, findings) = redact_text(&format!("token is {token} end"));
            assert!(out.contains("[REDACTED:"), "{token} not redacted: {out}");
            assert_eq!(findings.len(), 1, "expected 1 finding for {token}");
            assert_eq!(findings[0].rule, rule);
            assert!(!out.contains(token));
            assert!(out.contains("token is") && out.contains("end"));
        }
    }

    #[test]
    fn redacts_private_key_header() {
        let (out, findings) = redact_text("-----BEGIN RSA PRIVATE KEY-----");
        assert!(out.contains("[REDACTED:private-key-header]"), "got: {out}");
        assert_eq!(findings[0].rule, "private-key-header");
    }

    #[test]
    fn prefix_does_not_match_inside_word() {
        // `sk-` must NOT fire inside `task-force` or `risk-free`.
        for text in ["the task-force and risk-free disk", "task-sk-nope"] {
            let (out, findings) = redact_text(text);
            let has_sk = findings.iter().any(|f| f.rule == "openai-key");
            assert!(!has_sk, "sk- matched inside word in: {text:?} → {out}");
        }
    }

    // ── SendGrid + JWT ────────────────────────────────────────────────────────

    #[test]
    fn sendgrid_token_redacted() {
        // SG.<22alnum>.<43alnum> — the SG. prefix fires, body extends over dots/alnum.
        let key22 = "b".repeat(6) + "1C2D3E4F5G6H7I8J";
        let key43 = "a".repeat(10) + "B3x9Lp2Rt5Yv8Bn1Cx4Df6Qw7Zz0AaXkYm";
        let token = format!("SG.{key22}.{key43}");
        let (out, findings) = redact_text(&format!("api_key={token}"));
        assert!(!out.contains(&token), "SendGrid token leaked: {out}");
        assert!(
            !findings.is_empty(),
            "expected at least one finding for SendGrid token"
        );
    }

    #[test]
    fn jwt_redacted_via_prefix() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N";
        let (out, findings) = redact_text(&format!("Authorization: Bearer {jwt}"));
        assert!(out.contains("[REDACTED:jwt]"), "got: {out}");
        assert!(!out.contains("eyJhbGciOiJIUzI1NiJ9"), "JWT header leaked");
        assert!(
            findings.iter().any(|f| f.rule == "jwt"),
            "no jwt finding; findings: {findings:?}"
        );
    }

    // ── URL-credential ────────────────────────────────────────────────────────

    #[test]
    fn url_credential_basic() {
        let (out, findings) = redact_text("postgres://user:s3cr3tPass1@db.host.com:5432/mydb");
        assert!(
            !out.contains("s3cr3tPass1"),
            "connection-string password leaked: {out}"
        );
        assert!(
            findings
                .iter()
                .any(|f| f.rule == "connection-string-password"),
            "not categorised as connection-string-password; findings: {findings:?}"
        );
        assert!(
            !findings.iter().any(|f| f.rule == "email"),
            "password mis-categorised as email; findings: {findings:?}"
        );
    }

    #[test]
    fn url_credential_password_with_at_sign() {
        // Password contains a literal '@' — must still be fully redacted.
        let url = "amqp://guest:p@ss-w0rd@rabbit.local";
        let (out, findings) = redact_text(url);
        assert!(!out.contains("p@ss-w0rd"), "password with @ leaked: {out}");
        assert!(
            findings
                .iter()
                .any(|f| f.rule == "connection-string-password"),
            "not categorised as connection-string-password; findings: {findings:?}"
        );
        assert!(
            !findings.iter().any(|f| f.rule == "email"),
            "was mis-categorised as email; findings: {findings:?}"
        );
    }

    #[test]
    fn url_embedded_github_pat_redacted() {
        // ghp_ token embedded in a URL is caught by provider-prefix detector.
        let pat = "ghp_0123456789abcdefghij0123456789abcdef";
        let url = format!("https://x:{pat}@github.com/repo.git");
        let (out, findings) = redact_text(&url);
        assert!(!out.contains(pat), "URL-embedded ghp_ token leaked: {out}");
        // The URL-credential detector fires on the password range, which contains
        // the ghp_ token.  The provider-prefix or url-credential rule fires.
        assert!(
            findings
                .iter()
                .any(|f| f.rule == "github-pat" || f.rule == "connection-string-password"),
            "no finding for URL-embedded PAT; findings: {findings:?}"
        );
    }

    // ── Email ─────────────────────────────────────────────────────────────────

    #[test]
    fn redacts_email() {
        let (out, findings) = redact_text("contact craig@example.com please");
        assert!(out.contains("[REDACTED:email]"), "got: {out}");
        assert!(!out.contains("craig@example"));
        assert_eq!(findings[0].rule, "email");
        assert!(out.contains("contact") && out.contains("please"));
    }

    #[test]
    fn email_with_digit_in_local_part() {
        // user2024@example.com must redact as email, NOT connection-string-password.
        let (out, findings) = redact_text("send to user2024@example.com today");
        assert!(out.contains("[REDACTED:email]"), "got: {out}");
        assert!(!out.contains("user2024@example.com"), "email leaked: {out}");
        assert!(
            findings.iter().any(|f| f.rule == "email"),
            "not categorised as email; findings: {findings:?}"
        );
        assert!(
            !findings
                .iter()
                .any(|f| f.rule == "connection-string-password"),
            "wrongly categorised as connection-string-password; findings: {findings:?}"
        );
    }

    #[test]
    fn angle_bracket_email_redacted() {
        // `<user@example.com>` — angle brackets are boundary chars for the email detector.
        let (out, findings) = redact_text("From: <user@example.com> in header");
        assert!(
            !out.contains("user@example"),
            "angle-bracket email leaked: {out}"
        );
        assert!(
            findings.iter().any(|f| f.rule == "email"),
            "no email finding; findings: {findings:?}"
        );
    }

    #[test]
    fn localhost_not_email() {
        // admin@localhost has no dot in domain — must NOT redact.
        let text = "user admin@localhost here";
        let (out, findings) = redact_text(text);
        assert_eq!(out, text, "admin@localhost should not be redacted: {out}");
        assert!(
            !findings.iter().any(|f| f.rule == "email"),
            "admin@localhost wrongly flagged as email"
        );
    }

    #[test]
    fn bare_version_not_email() {
        // pkg@v1 has no dot in domain — must NOT redact.
        let text = "install pkg@v1 now";
        let (out, findings) = redact_text(text);
        assert_eq!(out, text, "pkg@v1 should not be redacted: {out}");
        assert!(
            !findings.iter().any(|f| f.rule == "email"),
            "pkg@v1 wrongly flagged"
        );
    }

    // ── Hex-in-context ────────────────────────────────────────────────────────

    #[test]
    fn bare_git_sha1_not_redacted() {
        // A bare 40-char SHA-1 in prose (no secret-indicator key) must NOT be redacted.
        let sha1 = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
        assert_eq!(sha1.len(), 40);
        let text = format!("commit {sha1} merged");
        let (out, findings) = redact_text(&text);
        assert_eq!(out, text, "bare git SHA-1 should not be redacted: {out}");
        assert!(
            !findings.iter().any(|f| f.rule == "hex-secret"),
            "bare SHA-1 wrongly flagged as hex-secret"
        );
    }

    #[test]
    fn bare_sha256_not_redacted() {
        // A bare 64-char SHA-256 in prose must NOT be redacted.
        let sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(sha256.len(), 64);
        let text = format!("checksum: {sha256}");
        let (out, findings) = redact_text(&text);
        assert_eq!(out, text, "bare SHA-256 should not be redacted: {out}");
        assert!(
            !findings.iter().any(|f| f.rule == "hex-secret"),
            "bare SHA-256 wrongly flagged as hex-secret"
        );
    }

    #[test]
    fn hex_after_api_key_is_redacted() {
        // Same hex, but after `api_key=` — must redact as hex-secret.
        let hex32 = "a3f1b2c4d5e6f7a8b9c0d1e2f3a4b5c6";
        assert_eq!(hex32.len(), 32);
        let (out, findings) = redact_text(&format!("api_key={hex32}"));
        assert!(!out.contains(hex32), "hex after api_key= leaked: {out}");
        assert!(
            findings.iter().any(|f| f.rule == "hex-secret"),
            "no hex-secret finding; findings: {findings:?}"
        );
    }

    #[test]
    fn hex_after_secret_key_is_redacted() {
        // `secret: <64-char hex>` — must redact as hex-secret.
        let hex64 = "a3f1b2c4d5e6f7a8b9c0d1e2f3a4b5c6a3f1b2c4d5e6f7a8b9c0d1e2f3a4b5c6";
        assert_eq!(hex64.len(), 64);
        let (out, findings) = redact_text(&format!("secret: {hex64}"));
        assert!(!out.contains(hex64), "hex after 'secret:' leaked: {out}");
        assert!(
            findings.iter().any(|f| f.rule == "hex-secret"),
            "no hex-secret finding; findings: {findings:?}"
        );
    }

    #[test]
    fn fix2_short_hex_not_redacted() {
        // Short hex strings (CSS colors, short hashes) must NOT be caught.
        for short in ["a3f1b2", "deadbeef", "cafebabe0000dead"] {
            let text = format!("color=#{short} or id={short}");
            let (out, findings) = redact_text(&text);
            assert_eq!(out, text, "short hex {short} should not be redacted");
            assert!(
                !findings.iter().any(|f| f.rule == "hex-secret"),
                "short hex {short} was incorrectly redacted"
            );
        }
    }

    // ── High-entropy ──────────────────────────────────────────────────────────

    #[test]
    fn redacts_high_entropy_token() {
        let secret = "aGVsbG8x9Zk3Qw7Lp2Rt5Yv8Bn1Cx4Df6"; // len>20, mixed case+digits
        let (out, findings) = redact_text(&format!("the token {secret} here"));
        assert!(out.contains("[REDACTED:high-entropy]"), "got: {out}");
        assert_eq!(findings[0].rule, "high-entropy");
        assert!(!out.contains(secret));
    }

    #[test]
    fn redacts_assignment_value_keeping_key() {
        let (out, findings) = redact_text("api_key=Xk9Lp2Rt5Yv8Bn1Cx4Df6Qw7Zz0Aa");
        assert!(
            out.starts_with("api_key="),
            "key name should survive: {out}"
        );
        assert!(out.contains("[REDACTED:high-entropy]"), "got: {out}");
        assert_eq!(findings.len(), 1);
    }

    // ── Prose untouched ───────────────────────────────────────────────────────

    #[test]
    fn leaves_prose_untouched() {
        let prose = "The quick brown fox jumps over the lazy dog repeatedly today.";
        let (out, _findings) = redact_text(prose);
        assert_eq!(out, prose, "prose was modified: {out}");
    }

    #[test]
    fn version_string_not_redacted() {
        let text = "version 1.2.3";
        let (out, _) = redact_text(text);
        assert_eq!(out, text, "version string should not be redacted");
    }

    #[test]
    fn file_path_not_redacted() {
        let text = "see src/foo/bar.rs";
        let (out, _) = redact_text(text);
        assert_eq!(out, text, "file path should not be redacted");
    }

    #[test]
    fn plain_https_url_not_redacted() {
        let text = "visit https://example.com for docs";
        let (out, _) = redact_text(text);
        assert_eq!(out, text, "plain URL should not be redacted");
    }

    #[test]
    fn hyphenated_words_not_redacted() {
        let text = "the task-force and risk-free disk";
        let (out, _) = redact_text(text);
        assert_eq!(out, text, "hyphenated prose should not be redacted");
    }

    // ── Structural preservation ───────────────────────────────────────────────

    #[test]
    fn preserves_whitespace_and_nonsecrets() {
        let text = "a\n  b\tc";
        let (out, _) = redact_text(text);
        assert_eq!(out, text);
    }

    #[test]
    fn redacts_secrets_embedded_in_json() {
        let line = r#"{"token":"ghp_0123456789abcdefghij0123456789abcdef","email":"a@b.com"}"#;
        let (out, findings) = redact_text(line);
        assert!(
            !out.contains("ghp_0123456789abcdefghij0123456789abcdef"),
            "json token leaked: {out}"
        );
        assert!(!out.contains("a@b"), "json email leaked: {out}");
        let rules: Vec<&str> = findings.iter().map(|f| f.rule.as_str()).collect();
        assert!(
            rules.contains(&"github-pat") && rules.contains(&"email"),
            "rules: {rules:?}"
        );
        assert!(
            out.contains("\"token\":") && out.contains("\"email\":"),
            "json structure lost"
        );
    }

    // ── Multibyte + empty ─────────────────────────────────────────────────────

    #[test]
    fn multibyte_tokens_no_panic() {
        // Non-ASCII tokens around an email must not panic and must survive.
        let (out, _) = redact_text("café résumé señor@example.com déjà");
        assert!(out.contains("[REDACTED:email]"), "got: {out}");
        assert!(out.contains("café") && out.contains("déjà"));
    }

    #[test]
    fn empty_input() {
        let (out, findings) = redact_text("");
        assert_eq!(out, "");
        assert!(findings.is_empty());
    }

    // ── Idempotency ───────────────────────────────────────────────────────────

    #[test]
    fn idempotent_placeholders_not_re_redacted() {
        let text = "the token aGVsbG8x9Zk3Qw7Lp2Rt5Yv8Bn1Cx4Df6 and a@b.com";
        let (once, _) = redact_text(text);
        let (twice, findings2) = redact_text(&once);
        assert_eq!(once, twice, "redaction must be idempotent");
        assert!(
            findings2.is_empty(),
            "[REDACTED:*] placeholders must not be re-redacted"
        );
    }

    // ── Reports ───────────────────────────────────────────────────────────────

    #[test]
    fn report_tallies_by_rule() {
        let entries = [
            "email me at a@b.com".to_owned(),
            "aws AKIAIOSFODNN7EXAMPLE here".to_owned(),
            "nothing secret here".to_owned(),
        ];
        let (redacted, report) = redact_entries(&entries);
        assert_eq!(report.entries_scanned, 3);
        assert_eq!(report.total, 2);
        assert_eq!(report.by_rule.get("email"), Some(&1));
        assert_eq!(report.by_rule.get("aws-access-key"), Some(&1));
        assert_eq!(redacted[2], "nothing secret here");
    }

    #[test]
    fn multiple_findings_one_line() {
        let (_out, findings) = redact_text("aws AKIAIOSFODNN7EXAMPLE and email a@b.com");
        assert_eq!(findings.len(), 2);
    }

    // ── Shannon entropy ───────────────────────────────────────────────────────

    #[test]
    fn entropy_basic() {
        assert!(shannon_entropy("").abs() < f64::EPSILON);
        assert!(shannon_entropy("aaaa").abs() < f64::EPSILON);
        assert!((shannon_entropy("abcd") - 2.0).abs() < 1e-9);
    }

    // ── Precision-fix regressions (re-verify pass: bare-prefix + substring-hex FPs) ──

    #[test]
    fn bare_sg_prefix_in_prose_not_redacted() {
        for prose in [
            "The org SG. handles the message.",
            "Region: SG.1 latency ok",
            "as shown in FIG. 3 and SG.2",
        ] {
            let (out, findings) = redact_text(prose);
            assert!(
                !findings.iter().any(|f| f.rule == "sendgrid"),
                "bare SG. wrongly redacted in {prose:?}: {out}"
            );
        }
    }

    #[test]
    fn eyj_word_not_redacted() {
        let (out, findings) = redact_text("the keyword eyJournal is not a jwt");
        assert!(
            !findings.iter().any(|f| f.rule == "jwt"),
            "ordinary word eyJournal wrongly redacted: {out}"
        );
    }

    #[test]
    fn real_sendgrid_and_jwt_still_redacted() {
        // The structure/length gate must NOT suppress genuine tokens.
        let sg = format!("SG.{}.{}", "a".repeat(22), "b".repeat(43));
        let (_o1, f1) = redact_text(&format!("key={sg}"));
        assert!(
            f1.iter().any(|f| f.rule == "sendgrid"),
            "real SendGrid missed"
        );
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N";
        let (_o2, f2) = redact_text(&format!("Bearer {jwt}"));
        assert!(f2.iter().any(|f| f.rule == "jwt"), "real JWT missed");
    }

    #[test]
    fn bare_hex_with_unrelated_key_substring_not_redacted() {
        let sha = "a3f1b2c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0"; // 40-char SHA-1
                                                              // `monkey` contains "key", `key.rs` mentions a key — but neither is the WORD
                                                              // adjacent to the hex, so the bare SHA must survive (no data loss).
        for line in [
            format!("monkey {sha} bars"),
            format!("modified key.rs: now points to {sha}"),
            format!("authentic {sha} ok"),
        ] {
            let (out, findings) = redact_text(&line);
            assert!(
                !findings.iter().any(|f| f.rule == "hex-secret"),
                "bare hex wrongly redacted in {line:?}: {out}"
            );
            assert!(out.contains(sha), "bare SHA must survive: {out}");
        }
    }

    #[test]
    fn hex_with_adjacent_key_still_redacted() {
        let sha = "a3f1b2c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0";
        for line in [format!("api_key={sha}"), format!("secret: {sha}")] {
            let (out, findings) = redact_text(&line);
            assert!(
                findings.iter().any(|f| f.rule == "hex-secret"),
                "context-gated hex missed in {line:?}: {out}"
            );
        }
    }

    // ── Author-seeded denylist (#51) ──────────────────────────────────────────
    //
    // For a controlled single-author corpus the signature gaps (bare hex ≡ checksum,
    // arbitrary low-entropy tokens) are closed by matching the author's *enumerated*
    // secrets literally — even with no prefix/context/entropy signal.

    #[test]
    fn denylist_catches_bare_hex_that_signatures_miss() {
        // Same shape as `bare_sha256_not_redacted`: a 64-char bare hex with no key
        // context. Signatures provably leave it alone — but if the author KNOWS this
        // value is a real secret (e.g. a bare API key that happens to be hex), the
        // denylist catches it.
        let secret = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let text = format!("checksum: {secret}");

        // Precondition: the existing pipeline does NOT catch it.
        let (baseline, _) = redact_text(&text);
        assert_eq!(
            baseline, text,
            "precondition: bare hex must be a signature gap"
        );

        // With the author's denylist, it IS redacted.
        let denylist = vec![secret.to_owned()];
        let (out, findings) = redact_text_with_denylist(&text, &denylist);
        assert!(
            !out.contains(secret),
            "denylist failed to redact bare hex: {out}"
        );
        assert!(
            findings.iter().any(|f| f.rule == "denylist"),
            "no denylist finding; findings: {findings:?}"
        );
        assert_eq!(out, "checksum: [REDACTED:denylist]");
    }

    #[test]
    fn denylist_catches_arbitrary_low_entropy_token() {
        // A short, low-entropy, no-prefix token: invisible to every signature detector.
        let secret = "hunter2-dev-pw";
        let text = format!("db password is {secret} ok");
        let (baseline, _) = redact_text(&text);
        assert_eq!(
            baseline, text,
            "precondition: low-entropy token must be a gap"
        );

        let (out, findings) = redact_text_with_denylist(&text, &[secret.to_owned()]);
        assert!(
            !out.contains(secret),
            "denylist missed low-entropy token: {out}"
        );
        assert!(findings.iter().any(|f| f.rule == "denylist"));
    }

    #[test]
    fn empty_denylist_is_backward_compatible() {
        // redact_text_with_denylist(text, &[]) must equal redact_text(text).
        let text = "api_key=a3f1b2c4d5e6f7a8b9c0d1e2f3a4b5c6 and craig@example.com";
        assert_eq!(redact_text_with_denylist(text, &[]), redact_text(text));
    }

    #[test]
    fn denylist_skips_short_entries_to_avoid_over_redaction() {
        // A stray short/blank line in the (gitignored) denylist source must NOT nuke
        // the corpus. Entries below DENYLIST_MIN_LEN are ignored.
        let text = "the cat sat on the mat".to_owned();
        let denylist = vec![String::new(), "a".to_owned(), "at".to_owned()];
        let (out, findings) = redact_text_with_denylist(&text, &denylist);
        assert_eq!(out, text, "short denylist entries over-redacted: {out}");
        assert!(findings.is_empty());
    }

    #[test]
    fn denylist_redacts_all_occurrences() {
        let secret = "s3cr3t-token-value";
        let text = format!("{secret} appears twice: {secret}");
        let (out, findings) = redact_text_with_denylist(&text, &[secret.to_owned()]);
        assert!(!out.contains(secret), "not all occurrences redacted: {out}");
        assert_eq!(findings.iter().filter(|f| f.rule == "denylist").count(), 2);
    }

    #[test]
    fn denylist_wins_priority_over_signature_rules() {
        // A denylisted value that also matches a signature rule is tagged `denylist`
        // (priority-0), not the signature rule — author knowledge is the strongest signal.
        let secret = "ghp_0123456789abcdefghij0123456789abcdef";
        let (_out, findings) = redact_text_with_denylist(secret, &[secret.to_owned()]);
        assert!(findings.iter().any(|f| f.rule == "denylist"));
        assert!(
            !findings.iter().any(|f| f.rule == "github-pat"),
            "signature rule won over denylist: {findings:?}"
        );
    }

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

    #[test]
    fn redact_entries_with_denylist_tallies_denylist_rule() {
        let secret = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let entries = vec![format!("bare: {secret}"), "clean line".to_owned()];
        let (redacted, report) = redact_entries_with_denylist(&entries, &[secret.to_owned()]);
        assert!(!redacted[0].contains(secret));
        assert_eq!(report.entries_scanned, 2);
        assert_eq!(report.by_rule.get("denylist"), Some(&1));
    }

    // ── Regression tests (adversarial review of #545) ─────────────────────────

    #[test]
    fn denylist_overlapping_literals_no_tail_leak() {
        // Finding 1: two enumerated secrets where one extends the other. The longer
        // secret's tail must NOT leak (was: first-writer-wins + binary overlap reject
        // dropped the longer match whole, leaving its tail bytes in the output).
        let dl = vec![
            "AKIASECRETONE".to_owned(),
            "AKIASECRETONEXTRATAILBYTES".to_owned(),
        ];
        let (out, _) = redact_text_with_denylist("key=AKIASECRETONEXTRATAILBYTES end", &dl);
        assert!(
            !out.contains("XTRATAILBYTES"),
            "longer secret tail leaked: {out}"
        );
        assert_eq!(out, "key=[REDACTED:denylist] end");
    }

    #[test]
    fn denylist_interior_overlap_no_leak() {
        // Finding 1 (distinct-secret variant): two secrets sharing an interior region,
        // neither a substring of the other. Merged coverage must redact the whole span.
        let dl = vec!["ABCDEFGHSECRET".to_owned(), "SECRETWXYZ1234".to_owned()];
        let (out, _) = redact_text_with_denylist("ABCDEFGHSECRETWXYZ1234", &dl);
        assert!(
            !out.contains("WXYZ1234"),
            "interior-overlap tail leaked: {out}"
        );
        assert_eq!(out, "[REDACTED:denylist]");
    }

    #[test]
    fn denylist_does_not_reredact_inside_placeholder() {
        // Finding 3: a denylist literal that collides with placeholder vocabulary must
        // not re-redact an existing [REDACTED:...] placeholder (idempotency contract).
        let already = "x [REDACTED:denylist] y";
        let (out, findings) = redact_text_with_denylist(already, &["denylist".to_owned()]);
        assert_eq!(out, already, "re-redacted inside placeholder: {out}");
        assert!(findings.is_empty());
    }

    #[test]
    fn denylist_redaction_is_idempotent() {
        // Re-running redaction on already-redacted output with the same denylist is stable.
        let secret = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let dl = vec![secret.to_owned()];
        let (once, _) = redact_text_with_denylist(&format!("bare {secret} here"), &dl);
        let (twice, findings2) = redact_text_with_denylist(&once, &dl);
        assert_eq!(once, twice, "second pass changed output");
        assert!(
            findings2.is_empty(),
            "second pass produced findings: {findings2:?}"
        );
    }

    #[test]
    fn denylist_honors_short_char_multibyte_secret() {
        // Finding 2: 3 chars but 9 bytes — the byte-length guard must honor it
        // (the old chars().count() guard wrongly skipped short-char/long-byte secrets).
        let secret = "€€€";
        assert_eq!(secret.chars().count(), 3);
        assert!(secret.len() >= DENYLIST_MIN_LEN);
        let (out, findings) =
            redact_text_with_denylist(&format!("pw={secret}"), &[secret.to_owned()]);
        assert!(!out.contains(secret), "multibyte secret survived: {out}");
        assert!(findings.iter().any(|f| f.rule == "denylist"));
    }

    #[test]
    fn denylist_min_len_boundary() {
        // Byte-length guard boundary: 3 bytes skipped, 4 bytes honored.
        let (out3, f3) = redact_text_with_denylist("abc xyz", &["abc".to_owned()]);
        assert_eq!(out3, "abc xyz");
        assert!(f3.is_empty());
        let (out4, f4) = redact_text_with_denylist("abcd xyz", &["abcd".to_owned()]);
        assert!(!out4.contains("abcd"));
        assert!(f4.iter().any(|f| f.rule == "denylist"));
    }
}
