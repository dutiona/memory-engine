//! Secrets/PII redaction gate (#45 S0.5).
//!
//! Sits in **front** of any bulk import of session `.jsonl` into ME/KB: 9 months of
//! dev sessions contain credentials, tokens and personal data. This is a HARD
//! prerequisite for the B3 backfill (#53) and is reviewed by the owner (#51) before
//! any backfill runs — so it is **build-only** here and is *not* wired into
//! `bootstrap_session` yet.
//!
//! Detection is dependency-free and operates on whitespace-delimited tokens, via
//! (1) known provider-token prefixes (gitleaks-style), (2) email PII, and (3) high
//! Shannon-entropy tokens (arbitrary secrets/keys).
//! Each finding is replaced with `[REDACTED:<rule>]` and tallied into an auditable
//! [`RedactionReport`]. (v1: #51 may upgrade to full regex/gitleaks rule sets, e.g.
//! `key = value` assignment patterns and JWT structure.)

use std::collections::{BTreeMap, HashMap};

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
];

/// A token is a high-entropy secret candidate only if at least this long.
const HIGH_ENTROPY_MIN_LEN: usize = 20;
/// Shannon entropy threshold (bits/char) above which a token-shaped string is redacted.
const HIGH_ENTROPY_BITS: f64 = 4.0;

/// One redaction: the rule that fired and the (now-removed) token.
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
    // Counts are tiny (token length); usize -> f64 cannot lose precision here.
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

/// True iff `token` looks like an email address (PII).
fn is_email(token: &str) -> bool {
    let Some(at) = token.find('@') else {
        return false;
    };
    let (local, domain) = (&token[..at], &token[at + 1..]);
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && token
            .chars()
            .all(|c| c.is_alphanumeric() || "._%+-@".contains(c))
}

/// True iff `token` uses a secret-like charset (base64/hex/url-safe with some digits
/// or specials) — so plain prose words are not flagged as high-entropy.
fn looks_like_secret_charset(token: &str) -> bool {
    let charset_ok = token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "+/=_-".contains(c));
    let has_digit = token.chars().any(|c| c.is_ascii_digit());
    let has_special = token.chars().any(|c| "+/=_-".contains(c));
    charset_ok && (has_digit || has_special)
}

/// Classify a token; returns the rule name if it is a secret/PII, else None.
fn classify(token: &str) -> Option<&'static str> {
    for (rule, prefix) in SECRET_PREFIXES {
        if token.starts_with(prefix) {
            return Some(rule);
        }
    }
    if is_email(token) {
        return Some("email");
    }
    if token.len() >= HIGH_ENTROPY_MIN_LEN
        && looks_like_secret_charset(token)
        && shannon_entropy(token) >= HIGH_ENTROPY_BITS
    {
        return Some("high-entropy");
    }
    None
}

fn flush_token(token: &mut String, out: &mut String, findings: &mut Vec<Finding>) {
    if token.is_empty() {
        return;
    }
    if let Some(rule) = classify(token) {
        out.push_str("[REDACTED:");
        out.push_str(rule);
        out.push(']');
        findings.push(Finding {
            rule: rule.to_owned(),
            token: std::mem::take(token),
        });
    } else {
        out.push_str(token);
        token.clear();
    }
}

/// Redact secrets/PII from `text`, preserving all non-secret characters + whitespace.
///
/// Returns the redacted text and the findings (secret tokens removed).
#[must_use]
pub fn redact_text(text: &str) -> (String, Vec<Finding>) {
    let mut out = String::with_capacity(text.len());
    let mut findings: Vec<Finding> = Vec::new();
    let mut token = String::new();
    for ch in text.chars() {
        if ch.is_whitespace() {
            flush_token(&mut token, &mut out, &mut findings);
            out.push(ch);
        } else {
            token.push(ch);
        }
    }
    flush_token(&mut token, &mut out, &mut findings);
    (out, findings)
}

/// Redact a sequence of session entries (raw `.jsonl` lines or extracted text),
/// returning the redacted entries and an auditable coverage report.
#[must_use]
pub fn redact_entries(entries: &[String]) -> (Vec<String>, RedactionReport) {
    let mut report = RedactionReport::default();
    let mut redacted = Vec::with_capacity(entries.len());
    for entry in entries {
        let (clean, findings) = redact_text(entry);
        report.entries_scanned += 1;
        for finding in &findings {
            report.record(&finding.rule);
        }
        redacted.push(clean);
    }
    (redacted, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_provider_prefixes() {
        for (token, rule) in [
            ("AKIAIOSFODNN7EXAMPLE", "aws-access-key"),
            ("ghp_0123456789abcdefghij0123456789abcdef", "github-pat"),
            ("sk-abcdefghijklmnopqrstuvwxyz0123", "openai-key"),
        ] {
            let (out, findings) = redact_text(&format!("token is {token} end"));
            assert!(out.contains("[REDACTED:"), "{token} not redacted: {out}");
            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].rule, rule);
            assert!(!out.contains(token));
            assert!(out.contains("token is") && out.contains("end"));
        }
    }

    #[test]
    fn redacts_private_key_header() {
        let (out, findings) = redact_text("-----BEGIN RSA PRIVATE KEY-----");
        assert!(out.contains("[REDACTED:private-key-header]"));
        assert_eq!(findings[0].rule, "private-key-header");
    }

    #[test]
    fn redacts_email() {
        let (out, findings) = redact_text("contact craig@example.com please");
        assert!(out.contains("[REDACTED:email]"));
        assert!(!out.contains("craig@example.com"));
        assert_eq!(findings[0].rule, "email");
        assert!(out.contains("contact") && out.contains("please"));
    }

    #[test]
    fn redacts_high_entropy_token() {
        let secret = "aGVsbG8x9Zk3Qw7Lp2Rt5Yv8Bn1Cx4Df6"; // len>20, mixed case+digits
        let (out, findings) = redact_text(&format!("the token {secret} here"));
        assert!(out.contains("[REDACTED:high-entropy]"), "got: {out}");
        assert_eq!(findings[0].rule, "high-entropy");
        assert!(!out.contains(secret));
    }

    #[test]
    fn leaves_prose_untouched() {
        let prose = "The quick brown fox jumps over the lazy dog repeatedly today.";
        let (out, findings) = redact_text(prose);
        assert_eq!(out, prose);
        assert!(findings.is_empty());
    }

    #[test]
    fn preserves_whitespace_and_nonsecrets() {
        let text = "a\n  b\tc";
        let (out, _) = redact_text(text);
        assert_eq!(out, text);
    }

    #[test]
    fn entropy_basic() {
        assert_eq!(shannon_entropy(""), 0.0);
        assert_eq!(shannon_entropy("aaaa"), 0.0); // single symbol -> 0 bits
        assert!((shannon_entropy("abcd") - 2.0).abs() < 1e-9); // 4 distinct -> log2(4) = 2
    }

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
}
