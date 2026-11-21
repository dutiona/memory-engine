//! Secret redaction shared by every write path (#1024).
//!
//! The rule set is the one memory-engine shares with knowledge-base: one pass of [`redact`]
//! reproduces every case of `tests/fixtures/redaction-cases.jsonl`, which both repositories
//! carry byte for byte (a rule change is a ME-then-KB pair of PRs quoting one `sha256sum`).
//! Write paths call [`scrub`], which repeats passes until one changes nothing: one pass can
//! leave a remainder the next one catches.
//!
//! Detectors, in priority order (an earlier one wins an overlap):
//! 0. the author's denylist (the inputs of bootstrap and of the backfill route, #1026);
//! 1. `machine-token`: `me_` and 32 base62 characters, with no `[0-9A-Za-z_]` on either side
//!    (ADR-0020 §4);
//! 2. provider prefixes: a known token prefix at a token boundary, 20 bytes or more in all
//!    (a `SG.` or `eyJ` token needs a dot past its prefix);
//! 3. `connection-string-password`: the password of a `user:pass@` URL authority;
//! 4. `hex-secret`: a run of 32+ hex digits whose preceding word holds a secret key word;
//! 5. `high-entropy`: a token of 20+ characters of the secret charset at 4.3 bits or more.
//!
//! A link (`scheme://` and RFC 3986 characters but `'(),;`) is exempt from every rule but
//! 0, 1 and 3, and its userinfo is not part of it (owner ruling, 2026-11-17). The exemption
//! covers a match that starts in the link's body, after its `://`: a token glued to a URL
//! (`AKIA…https://`, `ghp_…/https://`) is not the link's content. An email
//! address is not a secret. Each scan is one pass plus binary searches, so no input can
//! stall a write.
//!
//! A new write method must scrub its content and join the gated list of
//! `tests/redaction_write_paths.rs`, whose tripwire fails until it does.

use serde_json::Value;

use crate::error::{ConflictError, MemoryError, Result};
use crate::types::OutcomeClass;

mod sensitive_path;
pub use sensitive_path::is_sensitive_path;

/// The top-level event-payload member that holds ADR-0021 MC-9's capture context. When it has
/// the shape the MCP server writes, its strings get the machine-token rule only.
///
/// # Examples
///
/// ```
/// use memory_engine::redact::PROVENANCE_KEY;
///
/// let payload = serde_json::json!({ PROVENANCE_KEY: { "facts": {}, "dropped": {}, "key": null, "rules": 1 } });
/// assert!(payload.get("project").is_some());
/// ```
pub const PROVENANCE_KEY: &str = "project";

/// ADR-0020 §6's capture cap: tool input, tool result and tool error are cut to 512 bytes
/// of UTF-8, back to a character boundary, after redaction.
///
/// # Examples
///
/// ```
/// use memory_engine::redact::{CAPTURE_CAP, excerpt};
///
/// assert_eq!(excerpt(&"é".repeat(300))?.len(), CAPTURE_CAP);
/// # Ok::<(), memory_engine::MemoryError>(())
/// ```
pub const CAPTURE_CAP: usize = 512;

/// What a prefixed token's body may hold after its prefix.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Body {
    /// Alphanumerics and `. _ - + / =`.
    Token,
    /// ASCII alphanumerics only: the provider documents a base62 body, so an identifier such as
    /// `npm_config_user_agent` stays readable.
    Base62,
}

/// Known secret-token prefixes, in priority order: (rule, prefix, body). The rule names are
/// stored in placeholders, and knowledge-base's port carries the same table.
const SECRET_PREFIXES: &[(&str, &str, Body)] = &[
    ("aws-access-key", "AKIA", Body::Token),
    ("github-pat", "ghp_", Body::Token),
    ("github-fine-pat", "github_pat_", Body::Token),
    ("openai-key", "sk-", Body::Token),
    ("google-api-key", "AIza", Body::Token),
    ("slack-token", "xoxb-", Body::Token),
    ("slack-token", "xoxp-", Body::Token),
    ("private-key-header", "-----BEGIN", Body::Token),
    ("jwt", "eyJ", Body::Token),
    ("sendgrid", "SG.", Body::Token),
    ("huggingface-token", "hf_", Body::Base62),
    ("tailscale-key", "tskey-", Body::Token),
    ("gitlab-pat", "glpat-", Body::Token),
    ("github-token", "gho_", Body::Base62),
    ("github-token", "ghu_", Body::Base62),
    ("github-token", "ghs_", Body::Base62),
    ("github-token", "ghr_", Body::Base62),
    ("stripe-key", "sk_live_", Body::Base62),
    ("stripe-key", "rk_live_", Body::Base62),
    ("npm-token", "npm_", Body::Base62),
    ("pypi-token", "pypi-", Body::Token),
    ("age-secret-key", "AGE-SECRET-KEY-1", Body::Token),
    ("slack-token", "xapp-", Body::Token),
    ("supabase-token", "sbp_", Body::Base62),
    ("docker-pat", "dckr_pat_", Body::Token),
    ("digitalocean-token", "dop_v1_", Body::Base62),
];

/// A prefix matches only at the text start, after whitespace, or after one of these.
const PREFIX_BOUNDARY_CHARS: &[char] = &[
    '"', '\'', '`', '(', ')', '{', '}', '[', ']', ':', ',', ';', '=', '<', '>', '?', '&', '/', '@',
];

/// Besides whitespace, the characters that end a high-entropy token.
const ENTROPY_SPLIT_CHARS: &[char] = &[
    '"', '\'', '`', '{', '}', '[', ']', '(', ')', ':', ',', ';', '=',
];

/// The words that make a following hex run a secret, as a `_`-separated part of the word.
const HEX_CONTEXT_KEYS: &[&str] = &[
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
const LONGEST_KEY: usize = 10;

const HEX_SECRET_MIN_LEN: usize = 32;
const MIN_PREFIXED_SECRET_LEN: usize = 20;
const HIGH_ENTROPY_MIN_LEN: usize = 20;
const HIGH_ENTROPY_BITS: f64 = 4.3;
/// A denylist literal shorter than this many bytes is ignored: a stray short line in the
/// author's file must not redact the whole corpus.
const DENYLIST_MIN_LEN: usize = 4;
const MAX_SCRUB_PASSES: usize = 8;

const UNSETTLED: &str = "redaction did not settle; nothing was stored";
const CALLER_VECTOR: &str =
    "redaction would change the text this vector was computed from; scrub the text, then embed it";

/// Refuse a caller-supplied vector computed from text that redaction would change (plan decision
/// 7 of #1024): a vector of a secret must never be stored. Clean text passes unchanged.
pub(crate) fn require_clean(text: &str) -> Result<()> {
    if scrub(text)? == text {
        Ok(())
    } else {
        Err(MemoryError::Conflict(ConflictError::Redaction(
            CALLER_VECTOR,
        )))
    }
}

// ── Spans ────────────────────────────────────────────────────────────────────

/// Sorted, disjoint half-open byte spans with a logarithmic overlap test. A detector adds its
/// spans in increasing order to a pending run that [`Spans::flush`] merges in, so no span is
/// ever inserted mid-list.
#[derive(Default)]
struct Spans {
    starts: Vec<usize>,
    ends: Vec<usize>,
    rules: Vec<&'static str>,
    pending: Vec<(usize, usize, &'static str)>,
}

impl Spans {
    fn overlaps(&self, start: usize, end: usize) -> bool {
        let i = self.starts.partition_point(|&s| s < end);
        if i > 0 && self.ends[i - 1] > start {
            return true;
        }
        self.pending
            .last()
            .is_some_and(|&(s, e, _)| e > start && s < end)
    }

    /// Whether a flushed span holds byte `pos`.
    fn holds(&self, pos: usize) -> bool {
        let i = self.starts.partition_point(|&s| s <= pos);
        i > 0 && self.ends[i - 1] > pos
    }

    fn add(&mut self, start: usize, end: usize, rule: &'static str) {
        self.pending.push((start, end, rule));
    }

    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let mut all: Vec<(usize, usize, &'static str)> = self
            .starts
            .iter()
            .zip(&self.ends)
            .zip(&self.rules)
            .map(|((&s, &e), &r)| (s, e, r))
            .chain(self.pending.drain(..))
            .collect();
        all.sort_unstable();
        self.starts = all.iter().map(|m| m.0).collect();
        self.ends = all.iter().map(|m| m.1).collect();
        self.rules = all.iter().map(|m| m.2).collect();
    }
}

/// Maximal runs of one character class, looked up by byte position.
struct Runs {
    starts: Vec<usize>,
    ends: Vec<usize>,
}

impl Runs {
    fn new(text: &str, class: impl Fn(char) -> bool) -> Self {
        let (mut starts, mut ends) = (Vec::new(), Vec::new());
        let mut open: Option<usize> = None;
        for (i, c) in text.char_indices() {
            match (class(c), open) {
                (true, None) => open = Some(i),
                (false, Some(s)) => {
                    starts.push(s);
                    ends.push(i);
                    open = None;
                }
                _ => {}
            }
        }
        if let Some(s) = open {
            starts.push(s);
            ends.push(text.len());
        }
        Self { starts, ends }
    }

    /// The run holding byte `pos`, if any.
    fn run_at(&self, pos: usize) -> Option<(usize, usize)> {
        let i = self.starts.partition_point(|&s| s <= pos).checked_sub(1)?;
        (self.ends[i] > pos).then(|| (self.starts[i], self.ends[i]))
    }
}

fn positions(text: &str, needle: char) -> Vec<usize> {
    text.match_indices(needle).map(|(i, _)| i).collect()
}

fn char_before(text: &str, pos: usize) -> Option<char> {
    text[..pos].chars().next_back()
}

// ── Links ────────────────────────────────────────────────────────────────────

const fn is_link_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b':'
                | b'/'
                | b'?'
                | b'#'
                | b'['
                | b']'
                | b'@'
                | b'!'
                | b'$'
                | b'&'
                | b'*'
                | b'+'
                | b'='
                | b'%'
        )
}

/// `scheme://` plus link characters, and each link's body: what follows its `://`, or its
/// userinfo's last `@`. The userinfo is left out of both, so it stays subject to every rule. Only
/// a match that starts in a body is exempt: one that starts before it, in the scheme or glued to
/// it (`AKIA…https://`, `ghp_…/https://`), is not the link's content.
fn links(text: &str) -> (Spans, Spans) {
    let b = text.as_bytes();
    let mut links = Spans::default();
    let mut bodies = Spans::default();
    let mut pos = 0;
    while let Some(rel) = text[pos..].find("://") {
        let sep = pos + rel;
        let mut start = sep;
        while start > 0
            && (b[start - 1].is_ascii_alphanumeric() || matches!(b[start - 1], b'+' | b'.' | b'-'))
        {
            start -= 1;
        }
        while start < sep && !b[start].is_ascii_alphabetic() {
            start += 1;
        }
        // A placeholder right before `://` replaced a token glued to the scheme: it keeps the
        // link, so a later pass leaves the link's content alone.
        if start == sep
            && let Some(placeholder) = placeholder_ending_at(text, sep)
        {
            start = placeholder;
        }
        let mut end = sep + 3;
        while end < b.len() && is_link_byte(b[end]) {
            end += 1;
        }
        if start < sep {
            let authority_end = (sep + 3..end)
                .find(|&k| matches!(b[k], b'/' | b'?' | b'#'))
                .unwrap_or(end);
            if let Some(at) = text[sep + 3..authority_end].rfind('@') {
                links.add(start, sep + 3, "link");
                links.add(sep + 3 + at, end, "link");
                bodies.add(sep + 3 + at, end, "link");
            } else {
                links.add(start, end, "link");
                bodies.add(sep + 3, end, "link");
            }
        }
        pos = end.max(sep + 3);
    }
    links.flush();
    bodies.flush();
    (links, bodies)
}

// ── Detectors ────────────────────────────────────────────────────────────────

/// The author's literals: every occurrence, overlapping ones merged, none inside an existing
/// placeholder (so a second pass never re-redacts its own output).
fn detect_denylist(text: &str, denylist: &[String], spans: &mut Spans) {
    let placeholders = placeholder_spans(text);
    let inside_placeholder = |start: usize, end: usize| {
        let i = placeholders.partition_point(|&(s, _)| s < end);
        i > 0 && placeholders[i - 1].1 > start
    };
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for literal in denylist.iter().filter(|l| l.len() >= DENYLIST_MIN_LEN) {
        for (start, _) in text.match_indices(literal.as_str()) {
            let end = start + literal.len();
            if !inside_placeholder(start, end) {
                ranges.push((start, end));
            }
        }
    }
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (s, e) in ranges {
        match merged.last_mut() {
            Some(last) if s < last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    for (s, e) in merged {
        spans.add(s, e, "denylist");
    }
    spans.flush();
}

/// The rules whose placeholders the engine emits beside the provider prefixes'.
const FIXED_RULES: &[&str] = &[
    "denylist",
    "machine-token",
    "connection-string-password",
    "hex-secret",
    "high-entropy",
];

/// The start of a placeholder this engine emits, `[REDACTED:<rule>]` for one of its own rules,
/// ending at byte `end`. Any other bracketed text opens no link: the test is a fixed set of
/// ASCII suffixes, constant-time and the same in every engine whatever its string units.
fn placeholder_ending_at(text: &str, end: usize) -> Option<usize> {
    let head = &text[..end];
    let rest = head.strip_suffix(']')?;
    SECRET_PREFIXES
        .iter()
        .map(|&(rule, _, _)| rule)
        .chain(FIXED_RULES.iter().copied())
        .find_map(|rule| {
            rest.strip_suffix(rule)?
                .strip_suffix("[REDACTED:")
                .map(str::len)
        })
}

fn placeholder_spans(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = text[from..].find("[REDACTED:") {
        let start = from + rel;
        let Some(close) = text[start..].find(']') else {
            break;
        };
        let end = start + close + 1;
        out.push((start, end));
        from = end;
    }
    out
}

const fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn machine_token_at(b: &[u8], start: usize) -> bool {
    let end = start + 35;
    (start == 0 || !is_word_byte(b[start - 1]))
        && end <= b.len()
        && b[start + 3..end].iter().all(u8::is_ascii_alphanumeric)
        && (end == b.len() || !is_word_byte(b[end]))
}

/// The exact ADR-0020 shape, inside a link too: a machine token is never a published reference.
fn detect_machine_token(text: &str, spans: &mut Spans) {
    let b = text.as_bytes();
    let mut pos = 0;
    while let Some(rel) = text[pos..].find("me_") {
        let start = pos + rel;
        if machine_token_at(b, start) {
            if !spans.overlaps(start, start + 35) {
                spans.add(start, start + 35, "machine-token");
            }
            pos = start + 35;
        } else {
            pos = start + 1;
        }
    }
    spans.flush();
}

fn detect_provider_prefix(text: &str, spans: &mut Spans, links: &Spans, link_bodies: &Spans) {
    let b = text.as_bytes();
    let bodies = Runs::new(text, |c| {
        c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '/' | '=')
    });
    let dots = positions(text, '.');
    let newlines = positions(text, '\n');
    for &(rule, prefix, class) in SECRET_PREFIXES {
        let mut pos = 0;
        while let Some(rel) = text[pos..].find(prefix) {
            let start = pos + rel;
            pos = start + 1; // every prefix starts with an ASCII byte
            if char_before(text, start)
                .is_some_and(|c| !(c.is_whitespace() || PREFIX_BOUNDARY_CHARS.contains(&c)))
            {
                continue;
            }
            let end = if prefix == "-----BEGIN" {
                let j = newlines.partition_point(|&n| n < start);
                let mut end = newlines.get(j).copied().unwrap_or(text.len());
                if end > start && b[end - 1] == b'\r' {
                    end -= 1;
                }
                // A link on the line ends the header's span.
                let k = links.starts.partition_point(|&s| s <= start);
                if k < links.starts.len() && links.starts[k] < end {
                    end = links.starts[k];
                }
                end
            } else {
                let body = start + prefix.len();
                let end = match class {
                    Body::Token => bodies.run_at(body).map_or(body, |(_, e)| e),
                    Body::Base62 => {
                        body + b[body..]
                            .iter()
                            .take_while(|c| c.is_ascii_alphanumeric())
                            .count()
                    }
                };
                // A SendGrid key and a JWT are dotted: the dot must lie past the prefix.
                let dotless =
                    || dots.partition_point(|&d| d < end) == dots.partition_point(|&d| d < body);
                if end - start < MIN_PREFIXED_SECRET_LEN
                    || (matches!(prefix, "SG." | "eyJ") && dotless())
                {
                    continue;
                }
                end
            };
            if !spans.overlaps(start, end) && !link_bodies.holds(start) {
                spans.add(start, end, rule);
            }
        }
        spans.flush(); // each prefix's matches come in order; prefixes interleave
    }
}

/// The password spans of `user:pass@` authorities; links do not block this rule.
fn detect_url_credential(text: &str, spans: &Spans) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut pos = 0;
    while pos < text.len() {
        let Some(rel) = text[pos..].find("://") else {
            break;
        };
        let auth_start = pos + rel + 3;
        let auth_end = text[auth_start..]
            .find(|c: char| matches!(c, '/' | '?' | '#') || c.is_whitespace())
            .map_or(text.len(), |i| auth_start + i);
        let authority = &text[auth_start..auth_end];
        if let Some(at) = authority.rfind('@')
            && let Some(colon) = authority[..at].find(':')
        {
            let (start, end) = (auth_start + colon + 1, auth_start + at);
            if start < end && !spans.overlaps(start, end) {
                found.push((start, end));
            }
        }
        // Resume inside the authority, not past it: an authority without a path runs to the `/`
        // of the next `://` (`"https://a.com","db":"postgres://u:p@h"`), whose password the
        // next search must still reach. Each search starts past the last `://`, so the scan
        // stays linear.
        pos = auth_start;
    }
    found
}

fn is_key(part: &str) -> bool {
    part.len() <= LONGEST_KEY
        && part.is_ascii()
        && HEX_CONTEXT_KEYS.contains(&part.to_ascii_lowercase().as_str())
}

/// Word runs (alphanumeric or `_`) and, per `_`, whether a complete part of its word before it
/// is a key, so the context word is read in constant time per hex run.
struct Words<'a> {
    text: &'a str,
    runs: Runs,
    underscores: Vec<usize>,
    key_before: Vec<bool>,
}

impl<'a> Words<'a> {
    fn new(text: &'a str) -> Self {
        let runs = Runs::new(text, |c| c.is_alphanumeric() || c == '_');
        let (mut underscores, mut key_before) = (Vec::new(), Vec::new());
        for (&start, &end) in runs.starts.iter().zip(&runs.ends) {
            let (mut prev, mut flag) = (start, false);
            while let Some(rel) = text[prev..end].find('_') {
                let u = prev + rel;
                flag = flag || is_key(&text[prev..u]);
                underscores.push(u);
                key_before.push(flag);
                prev = u + 1;
            }
        }
        Self {
            text,
            runs,
            underscores,
            key_before,
        }
    }

    /// Whether `text[word_start..end]`, split on `_`, has a part that is a key.
    fn ends_with_key(&self, word_start: usize, end: usize) -> bool {
        let i = self.underscores.partition_point(|&u| u < end);
        if i > 0 && self.underscores[i - 1] >= word_start {
            let tail = self.underscores[i - 1] + 1;
            return self.key_before[i - 1]
                || (end - tail <= LONGEST_KEY && is_key(&self.text[tail..end]));
        }
        end - word_start <= LONGEST_KEY && is_key(&self.text[word_start..end])
    }
}

/// The word right before the hex run, on its line, past assignment punctuation, quotes and
/// whitespace, has a `_`-separated part that is a context key.
fn has_hex_context_key(text: &str, hex_start: usize, line_start: usize, words: &Words<'_>) -> bool {
    let mut p = hex_start;
    while p > line_start {
        match char_before(text, p) {
            Some(c) if c.is_whitespace() || "=:\"'`".contains(c) => p -= c.len_utf8(),
            _ => break,
        }
    }
    p > line_start
        && words
            .runs
            .run_at(p - 1)
            .is_some_and(|(start, _)| words.ends_with_key(start.max(line_start), p))
}

fn detect_hex_in_context(text: &str, spans: &mut Spans, link_bodies: &Spans) {
    let b = text.as_bytes();
    let words = Words::new(text);
    let newlines = positions(text, '\n');
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_hexdigit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_hexdigit() {
            i += 1;
        }
        if i - start < HEX_SECRET_MIN_LEN {
            continue;
        }
        let j = newlines.partition_point(|&n| n < start);
        let line_start = if j > 0 { newlines[j - 1] + 1 } else { 0 };
        if has_hex_context_key(text, start, line_start, &words)
            && !spans.overlaps(start, i)
            && !link_bodies.holds(start)
        {
            spans.add(start, i, "hex-secret");
        }
    }
    spans.flush();
}

/// Shannon entropy in bits per byte of an ASCII token, summed in first-occurrence order.
#[allow(clippy::suboptimal_flops)]
fn shannon_entropy(token: &[u8]) -> f64 {
    let mut counts = [0_u32; 128];
    for &b in token {
        counts[usize::from(b & 0x7f)] += 1;
    }
    #[allow(clippy::cast_precision_loss)]
    let n = token.len() as f64;
    let mut seen = [false; 128];
    let mut sum = 0.0;
    for &b in token {
        let k = usize::from(b & 0x7f);
        if !std::mem::replace(&mut seen[k], true) {
            let p = f64::from(counts[k]) / n;
            sum += p * p.log2(); // knowledge-base's float order, not `mul_add`'s rounding
        }
    }
    -sum
}

const fn is_secret_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-')
}

fn detect_high_entropy(text: &str, spans: &mut Spans, link_bodies: &Spans) {
    let tokens = Runs::new(text, |c| {
        !(c.is_whitespace() || ENTROPY_SPLIT_CHARS.contains(&c))
    });
    for (&start, &end) in tokens.starts.iter().zip(&tokens.ends) {
        let token = &text.as_bytes()[start..end];
        if token.len() >= HIGH_ENTROPY_MIN_LEN
            && token.iter().copied().all(is_secret_byte)
            && token
                .iter()
                .any(|&b| b.is_ascii_digit() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-'))
            && shannon_entropy(token) >= HIGH_ENTROPY_BITS
            && !spans.overlaps(start, end)
            && !link_bodies.holds(start)
        {
            spans.add(start, end, "high-entropy");
        }
    }
    spans.flush();
}

// ── Passes ───────────────────────────────────────────────────────────────────

fn collect(text: &str, denylist: &[String]) -> Spans {
    let (links, link_bodies) = links(text);
    let mut spans = Spans::default();
    if !denylist.is_empty() {
        detect_denylist(text, denylist, &mut spans);
    }
    detect_machine_token(text, &mut spans);
    detect_provider_prefix(text, &mut spans, &links, &link_bodies);
    for (start, end) in detect_url_credential(text, &spans) {
        spans.add(start, end, "connection-string-password");
    }
    spans.flush();
    detect_hex_in_context(text, &mut spans, &link_bodies);
    detect_high_entropy(text, &mut spans, &link_bodies);
    spans
}

/// One pass: the rebuilt text, the rules in text order, and how many matches were not already
/// their own placeholder.
fn one_pass(text: &str, denylist: &[String]) -> (String, Vec<&'static str>, u64) {
    let spans = collect(text, denylist);
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut found = 0;
    for ((&start, &end), &rule) in spans.starts.iter().zip(&spans.ends).zip(&spans.rules) {
        out.push_str(&text[cursor..start]);
        let placeholder = format!("[REDACTED:{rule}]");
        // A match that is already a placeholder is a relabel (a token's placeholder becoming the
        // URL password it sits in), not a new secret.
        let matched = &text[start..end];
        if !(matched.starts_with("[REDACTED:") && matched.ends_with(']')) {
            found += 1;
        }
        out.push_str(&placeholder);
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    (out, spans.rules, found)
}

/// One pass of the shared rule set: `text` with each match replaced by `[REDACTED:<rule>]`,
/// and the rules in text order. This is the function the shared fixture pins; write paths use
/// [`scrub`].
///
/// # Examples
///
/// ```
/// use memory_engine::redact::redact;
///
/// let aws = format!("AKIA{}", "IOSFODNN7EXAMPLE");
/// let (out, rules) = redact(&format!("key {aws} and craig@example.com"));
/// assert_eq!(out, "key [REDACTED:aws-access-key] and craig@example.com");
/// assert_eq!(rules, ["aws-access-key"]);
/// ```
#[must_use]
pub fn redact(text: &str) -> (String, Vec<&'static str>) {
    let (out, rules, _) = one_pass(text, &[]);
    (out, rules)
}

fn scrub_passes(text: &str, denylist: &[String], max_passes: usize) -> Result<(String, u64)> {
    let mut current = text.to_owned();
    let mut found = 0;
    for _ in 0..max_passes {
        let (again, _, n) = one_pass(&current, denylist);
        if again == current {
            return Ok((current, found));
        }
        found += n;
        current = again;
    }
    Err(MemoryError::Conflict(ConflictError::Redaction(UNSETTLED)))
}

/// Redact `text` until a pass changes nothing, with the author's `denylist`: the scrubbed text
/// and the number of secrets removed.
pub(crate) fn scrub_with(text: &str, denylist: &[String]) -> Result<(String, u64)> {
    scrub_passes(text, denylist, MAX_SCRUB_PASSES)
}

/// Redact `text` until a pass changes nothing: what every write path stores.
///
/// # Errors
///
/// Returns `Conflict(Redaction)` when the text does not settle within the pass limit; nothing
/// may then be stored.
///
/// # Examples
///
/// ```
/// use memory_engine::redact::scrub;
///
/// let text = format!("api_key=a3f1b2c4d5e6f7a8b9c0d1e2f3a4b5c6{}", "qWeRtYuIoPaSdFgHjKl9876");
/// assert_eq!(scrub(&text)?, "api_key=[REDACTED:hex-secret][REDACTED:high-entropy]");
/// # Ok::<(), memory_engine::MemoryError>(())
/// ```
pub fn scrub(text: &str) -> Result<String> {
    scrub_passes(text, &[], MAX_SCRUB_PASSES).map(|(s, _)| s)
}

/// The machine-token rule alone, for capture-context strings (decision 5 of #1024's plan): the
/// other rules misread ordinary paths. One pass settles.
///
/// # Examples
///
/// ```
/// use memory_engine::redact::scrub_provenance;
///
/// let token = format!("me_{}", "Zq7Xw3Rt9Lp2Vb6Nm4Kc8Hd1Fs5Gj0Ay");
/// assert_eq!(scrub_provenance(&format!("/dev/sk-learn-tutorial-2024/{token}")),
///            "/dev/sk-learn-tutorial-2024/[REDACTED:machine-token]");
/// ```
#[must_use]
pub fn scrub_provenance(text: &str) -> String {
    machine_tokens(text).0
}

fn machine_tokens(text: &str) -> (String, u64) {
    let mut spans = Spans::default();
    detect_machine_token(text, &mut spans);
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for (&start, &end) in spans.starts.iter().zip(&spans.ends) {
        out.push_str(&text[cursor..start]);
        out.push_str("[REDACTED:machine-token]");
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    (out, u64::try_from(spans.starts.len()).unwrap_or(u64::MAX))
}

// ── JSON ─────────────────────────────────────────────────────────────────────

/// The key's last word (alphanumerics and `_`), split on `_`, holds a context key: the key of
/// a JSON member is the context word of a hex value under it.
fn key_has_context(key: &str) -> bool {
    let word_len: usize = key
        .chars()
        .rev()
        .take_while(|&c| c.is_alphanumeric() || c == '_')
        .map(char::len_utf8)
        .sum();
    key[key.len() - word_len..].split('_').any(is_key)
}

fn scrub_member(key: &str, value: &mut Value, denylist: &[String]) -> Result<u64> {
    if let Value::String(s) = value
        && s.len() >= HEX_SECRET_MIN_LEN
        && s.bytes().all(|b| b.is_ascii_hexdigit())
        && key_has_context(key)
    {
        "[REDACTED:hex-secret]".clone_into(s);
        return Ok(1);
    }
    scrub_json(value, denylist)
}

/// Scrub every string leaf of `value` in place; keys and non-strings are kept. A hex leaf whose
/// key is a context word is redacted too. Returns the number of secrets removed.
pub(crate) fn scrub_json(value: &mut Value, denylist: &[String]) -> Result<u64> {
    match value {
        Value::String(s) => {
            let (clean, n) = scrub_with(s, denylist)?;
            if n > 0 || clean != *s {
                *s = clean;
            }
            Ok(n)
        }
        Value::Array(items) => items.iter_mut().map(|v| scrub_json(v, denylist)).sum(),
        Value::Object(map) => map
            .iter_mut()
            .map(|(k, v)| scrub_member(k, v, denylist))
            .sum(),
        _ => Ok(0),
    }
}

const FACT_NAMES: &[&str] = &["root", "dev_root", "upstream_url", "origin_url", "branch"];
const MAX_KEY_BYTES: usize = 256;

/// Exactly the shape MCP's `ProjectFacts::provenance` writes.
fn is_provenance(value: &Value) -> bool {
    let Value::Object(map) = value else {
        return false;
    };
    let strings = |v: Option<&Value>, names: Option<&[&str]>| {
        v.and_then(Value::as_object).is_some_and(|m| {
            m.iter()
                .all(|(k, v)| v.is_string() && names.is_none_or(|n| n.contains(&k.as_str())))
        })
    };
    map.len() == 4
        && strings(map.get("facts"), Some(FACT_NAMES))
        && strings(map.get("dropped"), None)
        && map
            .get("key")
            .is_some_and(|k| k.is_null() || k.as_str().is_some_and(|k| k.len() <= MAX_KEY_BYTES))
        && map.get("rules").is_some_and(Value::is_number)
}

fn scrub_provenance_value(value: &mut Value) -> u64 {
    match value {
        Value::String(s) => {
            let (clean, n) = machine_tokens(s);
            *s = clean;
            n
        }
        Value::Object(map) => map.values_mut().map(scrub_provenance_value).sum(),
        _ => 0,
    }
}

/// The members of a captured event's payload that are identifiers, not content. The author's
/// denylist holds host names, which are machine names: applied here it would hide a row from
/// `erase --machine` (#1026). Every other rule still applies to them.
const IDENTIFIER_MEMBERS: &[&str] = &[
    "machine",
    "event_id",
    "time",
    "seq",
    "harness",
    "client_version",
    "harness_version",
    "kind",
    "actor",
    "agent_id",
    "agent_type",
];

/// Scrub an event payload: its genuine capture context (`PROVENANCE_KEY`, in MC-9's exact
/// shape) gets the machine-token rule only, its identifier members every rule but the
/// `denylist`, everything else every rule and the `denylist`.
pub(crate) fn scrub_payload(payload: &mut Value, denylist: &[String]) -> Result<u64> {
    let Value::Object(map) = payload else {
        return scrub_json(payload, denylist);
    };
    let mut found = 0;
    for (k, v) in map.iter_mut() {
        found += if k == PROVENANCE_KEY && is_provenance(v) {
            scrub_provenance_value(v)
        } else if IDENTIFIER_MEMBERS.contains(&k.as_str()) {
            scrub_member(k, v, &[])?
        } else {
            scrub_member(k, v, denylist)?
        };
    }
    Ok(found)
}

// ── The capture cut ──────────────────────────────────────────────────────────

/// At most `max` bytes of `text`, back to a character boundary.
fn head(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// At most [`CAPTURE_CAP`] bytes of already-scrubbed `text`, back to a character boundary.
pub(crate) fn cut(text: &str) -> &str {
    head(text, CAPTURE_CAP)
}

/// The part of a tool text that is scanned: the whole of one within the write limit, else its
/// first `MAX_PAYLOAD_BYTES`. Only the first [`CAPTURE_CAP`] bytes of what is scanned are kept,
/// and a secret that starts there ends far below the window's end.
fn window(text: &str) -> &str {
    head(text, crate::limits::MAX_PAYLOAD_BYTES)
}

/// Already-scrubbed `text` cut to [`CAPTURE_CAP`] and settled. A cut inside a word can expose a
/// match the whole word did not hold (its first 20 characters alone can score as high-entropy),
/// so the cut is scrubbed and cut again until a scrub changes nothing: a later scrub of the
/// stored excerpt can then never grow it past the cap. Such a match is an artefact of the cut,
/// not a secret of the text, so it is not counted.
pub(crate) fn settled_cut(text: &str) -> Result<String> {
    let mut out = cut(text).to_owned();
    for _ in 0..MAX_SCRUB_PASSES {
        let next = scrub(&out)?;
        if next == out {
            return Ok(out);
        }
        cut(&next).clone_into(&mut out);
    }
    Err(MemoryError::Conflict(ConflictError::Redaction(UNSETTLED)))
}

/// A scrubbed tool input: kept as JSON when its compact form fits the cap, else a JSON string
/// holding the settled cut of that form (`compact`, which the caller already has).
pub(crate) fn cut_json(value: Value, compact: &str) -> Result<Value> {
    Ok(if compact.len() <= CAPTURE_CAP {
        value
    } else {
        Value::String(settled_cut(compact)?)
    })
}

/// ADR-0020 §6's excerpt: `text` redacted, then cut to [`CAPTURE_CAP`] bytes. Redacting first
/// keeps the cut from slicing a secret below its pattern.
///
/// # Errors
///
/// Returns `Conflict(Redaction)` when the text does not settle.
///
/// # Examples
///
/// ```
/// use memory_engine::redact::{CAPTURE_CAP, excerpt};
///
/// let token = format!("ghp_{}", "0123456789abcdefghij".repeat(2));
/// let text = format!("{} {token}", "x".repeat(500));
/// let cut = excerpt(&text)?;
/// assert!(cut.len() <= CAPTURE_CAP && !cut.contains("ghp_"));
/// # Ok::<(), memory_engine::MemoryError>(())
/// ```
pub fn excerpt(text: &str) -> Result<String> {
    settled_cut(&scrub(text)?)
}

// ── Tool calls (ADR-0020 §6) ─────────────────────────────────────────────────

/// What of a tool call is stored, under the capture contract (#1024).
pub(crate) struct ToolCapture {
    /// The stored input: scrubbed and cut, or a stub's path.
    pub args: Value,
    /// The stored result: scrubbed and cut; `None` for a stub.
    pub result: Option<String>,
    /// The scrubbed, uncut result a promotion may quote; `None` for a stub or a result over the
    /// write limit.
    pub full_result: Option<String>,
    /// What deduplication hashes: the scrubbed uncut input, or a domain-separated stand-in.
    pub hash_input: String,
    /// The stored outcome: an `Other` text is scrubbed and cut like the result.
    pub outcome: OutcomeClass,
    /// Secrets removed from the input (with the outcome) and from the result, apart: a
    /// deduplicated call rewrites its row's result but its input and outcome only matched it.
    pub input_found: u64,
    pub result_found: u64,
    /// Whether the call touched a sensitive path.
    pub stub: bool,
}

/// Apply the capture contract to one tool call: a call touching a sensitive path is a stub that
/// keeps its token and outcome and nothing of its input or result; otherwise the input and
/// result are scrubbed, then cut to [`CAPTURE_CAP`]. An `Other` outcome text is scrubbed and cut
/// on both branches. A field over the write limit keeps an excerpt of its first window, scrubbed
/// and cut the same way: the rest is never scanned, an input that large never deduplicates,
/// and a result that large is never quoted. Every text also loses the author's `denylist`.
pub(crate) fn capture_tool_use(
    args: &Value,
    result: Option<&str>,
    outcome: OutcomeClass,
    touched_paths: &[String],
    cwd: Option<&str>,
    denylist: &[String],
) -> Result<ToolCapture> {
    let (outcome, outcome_found) = match outcome {
        OutcomeClass::Other(text) => {
            let (text, found) = capture_text(&text, denylist)?;
            (OutcomeClass::Other(text), found)
        }
        named => (named, 0),
    };
    let sensitive = |token: &String| {
        is_sensitive_path(token)
            || cwd.is_some_and(|cwd| {
                !token.starts_with(['/', '~']) && is_sensitive_path(&format!("{cwd}/{token}"))
            })
    };
    if let Some(token) = touched_paths.iter().find(|t| sensitive(t)) {
        let (token, found) = scrub_with(token, denylist)?;
        return Ok(ToolCapture {
            args: serde_json::json!({ "sensitive_path": settled_cut(&token)? }),
            result: None,
            full_result: None,
            hash_input: format!("stub\0{token}"),
            outcome,
            input_found: found + outcome_found,
            result_found: 0,
            stub: true,
        });
    }
    let (args, hash_input, input_found) =
        if crate::limits::check_json_size(args, "tool input").is_err() {
            // A per-call nonce from the standard library's OS-seeded hasher keys: no digest of the
            // unscanned content is kept, and the row never matches another call or a replay.
            let nonce = std::hash::BuildHasher::hash_one(
                &std::collections::hash_map::RandomState::new(),
                std::time::SystemTime::now(),
            );
            let (clean, found) = scrub_with(window(&args.to_string()), denylist)?;
            (
                Value::String(settled_cut(&clean)?),
                format!("oversized\0{nonce}"),
                found,
            )
        } else {
            let mut args = args.clone();
            let found = scrub_json(&mut args, denylist)?;
            let hash_input = args.to_string();
            (cut_json(args, &hash_input)?, hash_input, found)
        };
    let (result, full_result, result_found) = match result {
        None => (None, None, 0),
        Some(r) if crate::limits::check_str_size(r, "tool result").is_err() => {
            let (clean, found) = scrub_with(window(r), denylist)?;
            (Some(settled_cut(&clean)?), None, found)
        }
        Some(r) => {
            let (clean, found) = scrub_with(r, denylist)?;
            (Some(settled_cut(&clean)?), Some(clean), found)
        }
    };
    Ok(ToolCapture {
        args,
        result,
        full_result,
        hash_input,
        outcome,
        input_found: input_found + outcome_found,
        result_found,
        stub: false,
    })
}

/// A tool text as stored: scrubbed, then cut; over the write limit, only its first window is
/// scanned. Returns the text and its findings.
fn capture_text(text: &str, denylist: &[String]) -> Result<(String, u64)> {
    let (clean, found) = scrub_with(window(text), denylist)?;
    Ok((settled_cut(&clean)?, found))
}

// ── Derived text a consumer embeds itself ────────────────────────────────────

/// A [`DeltaProposer`](me_traits::DeltaProposer) whose summaries are scrubbed (#1024).
///
/// `LlmDreamCycle` embeds its proposer's summaries inside me-cognitive, below the facade's
/// gate. A group whose summary exceeds the write limit, or does not settle, is dropped: it
/// could never be stored.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// use memory_engine::redact::ScrubbingProposer;
/// use memory_engine::traits::DeltaProposer;
/// use memory_engine::types::{ConsolidationProposal, Fact, MergeGroup};
///
/// struct One;
/// impl DeltaProposer for One {
///     fn propose(&self, _: &[Fact], _: &[Fact]) -> memory_engine::error::Result<ConsolidationProposal> {
///         let summary = format!("merged, key AKIA{}", "IOSFODNN7EXAMPLE");
///         Ok(ConsolidationProposal { merges: vec![MergeGroup { source_ids: vec![1, 2], summary }] })
///     }
/// }
///
/// let proposal = ScrubbingProposer(Arc::new(One)).propose(&[], &[])?;
/// assert_eq!(proposal.merges[0].summary, "merged, key [REDACTED:aws-access-key]");
/// # Ok::<(), memory_engine::MemoryError>(())
/// ```
pub struct ScrubbingProposer(pub std::sync::Arc<dyn me_traits::DeltaProposer>);

impl me_traits::DeltaProposer for ScrubbingProposer {
    fn propose(
        &self,
        window: &[me_types::types::Fact],
        prior_wisdom: &[me_types::types::Fact],
    ) -> Result<me_types::types::ConsolidationProposal> {
        let mut proposal = self.0.propose(window, prior_wisdom)?;
        proposal.merges.retain_mut(|group| {
            if group.summary.len() > crate::limits::MAX_PAYLOAD_BYTES {
                return false;
            }
            scrub(&group.summary)
                .map(|clean| group.summary = clean)
                .is_ok()
        });
        Ok(proposal)
    }
}

#[cfg(test)]
mod tests;
