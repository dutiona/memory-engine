//! The request's project facts (ADR-0020 §3) and the project key derived from them (§7).
//!
//! The five `ME-Project-*` headers are decoded once, canonically: a bad fact is dropped and
//! flagged, never the request. Both remote URLs lose their userinfo before anything holds
//! them. The key follows rules version [`KEY_RULES_VERSION`], stated canonically in
//! `docs/reference/mcp-server.md`. Nothing here depends on MCP, so the lifecycle routes
//! (#1025) decode and derive the same way.

use std::collections::BTreeMap;
use std::fmt;

use axum::http::HeaderMap;
use memory_engine::types::MAX_SEGMENT_LEN;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::auth::Machine;

/// The version of the key rules below; any change to them increments it (ADR-0020 §7).
pub const KEY_RULES_VERSION: u32 = 1;

/// Why a fact was dropped (ADR-0020 §3). These names are stored in the event log, so they
/// never change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Flaw {
    /// The header was sent more than once.
    Repeated,
    /// The header was sent empty; a fact that does not exist has no header.
    Empty,
    /// The value exceeds 4,096 bytes.
    TooLong,
    /// The value holds a byte outside 0x21 to 0x7E.
    RawByte,
    /// A `%` is not followed by two hex digits.
    BadEscape,
    /// The decoded bytes are not UTF-8.
    NotUtf8,
    /// The decoded text holds a control character.
    Control,
    /// The dev root is not an absolute path of plain ASCII without `%`, `"` or `\`.
    NotPlainAbsolute,
    /// A remote URL still holds an `@` after stripping and gives no remote key: userinfo an
    /// encoded `/` or `@` hid from the strip, or a keyless URL with an `@` in its path; dropped
    /// rather than stored, which changes no key.
    UnstrippedUserinfo,
}

/// ADR-0020 §3: each header value is at most 4,096 bytes.
const MAX_FACT_BYTES: usize = 4096;

/// The header each fact travels under, and its name in the record.
const HEADERS: [(&str, &str); 5] = [
    ("me-project-root", "root"),
    ("me-project-dev-root", "dev_root"),
    ("me-project-upstream-url", "upstream_url"),
    ("me-project-origin-url", "origin_url"),
    ("me-project-branch", "branch"),
];

/// The project facts one request carried, decoded, userinfo removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectFacts {
    /// The project root: the main working tree's top level, or the project directory.
    pub root: Option<String>,
    /// The machine's dev root, as configured.
    pub dev_root: Option<String>,
    /// The `upstream` remote's URL.
    pub upstream_url: Option<String>,
    /// The `origin` remote's URL.
    pub origin_url: Option<String>,
    /// The checked-out branch; provenance only.
    pub branch: Option<String>,
    /// Each dropped fact, by its record name, with the reason.
    pub dropped: BTreeMap<&'static str, Flaw>,
}

impl ProjectFacts {
    /// Decode the five facts from `headers`. Never fails: a bad fact is dropped and flagged.
    #[must_use]
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let mut facts = Self::default();
        for (header, name) in HEADERS {
            let mut values = headers.get_all(header).iter();
            let Some(first) = values.next() else {
                continue;
            };
            let decoded = if values.next().is_some() {
                Err(Flaw::Repeated)
            } else if name == "dev_root" {
                dev_root(first.as_bytes())
            } else {
                decode(first.as_bytes())
            };
            match decoded {
                Ok(value) if name.ends_with("_url") => {
                    let value = strip_userinfo(&value);
                    if unstripped_userinfo(&value) {
                        facts.dropped.insert(name, Flaw::UnstrippedUserinfo);
                    } else {
                        *facts.slot(name) = Some(value);
                    }
                }
                Ok(value) => *facts.slot(name) = Some(value),
                Err(flaw) => {
                    facts.dropped.insert(name, flaw);
                }
            }
        }
        facts
    }

    /// The project key, by rules version 1: the `upstream` remote's, else the `origin`
    /// remote's, else the path key; `None` when the chosen key exceeds the segment limit (no
    /// fall-through to the next rule) or nothing gives one.
    #[must_use]
    pub fn key(&self, machine: &Machine) -> Option<ProjectKey> {
        let candidate = self
            .upstream_url
            .as_deref()
            .and_then(remote_key)
            .or_else(|| self.origin_url.as_deref().and_then(remote_key))
            .or_else(|| path_key(machine, self.root.as_deref()?, self.dev_root.as_deref()?))?;
        ProjectKey::new(&candidate)
    }

    /// The block every call record keeps (ADR-0020 §8): the facts as decoded, the dropped
    /// ones, the key and the rules version. All four members are always present.
    #[must_use]
    pub fn provenance(&self, key: Option<&ProjectKey>) -> Value {
        let mut facts = Map::new();
        for (name, value) in [
            ("root", &self.root),
            ("dev_root", &self.dev_root),
            ("upstream_url", &self.upstream_url),
            ("origin_url", &self.origin_url),
            ("branch", &self.branch),
        ] {
            if let Some(value) = value {
                facts.insert(name.to_owned(), value.as_str().into());
            }
        }
        json!({
            "facts": facts,
            "dropped": self.dropped,
            "key": key.map(ProjectKey::as_str),
            "rules": KEY_RULES_VERSION,
        })
    }

    fn slot(&mut self, name: &str) -> &mut Option<String> {
        match name {
            "root" => &mut self.root,
            "dev_root" => &mut self.dev_root,
            "upstream_url" => &mut self.upstream_url,
            "origin_url" => &mut self.origin_url,
            _ => &mut self.branch,
        }
    }
}

/// A project's key, with its scope label: it exists only when `project:<label>` fits the
/// scope tree's segment limit (ADR-0020 §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectKey {
    key: String,
    label: String,
}

impl ProjectKey {
    /// The key, or `None` when it is empty or its segment exceeds 256 bytes.
    #[must_use]
    pub fn new(key: &str) -> Option<Self> {
        let label = label(key);
        (!key.is_empty() && "project:".len() + label.len() <= MAX_SEGMENT_LEN).then(|| Self {
            key: key.to_owned(),
            label,
        })
    }

    /// The key, as `recall` shows it in a result's scope.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.key
    }

    /// The scope label: every byte outside `A-Z a-z 0-9 - . _ ~` as uppercase `%HH`.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }
}

impl fmt::Display for ProjectKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.key)
    }
}

fn label(key: &str) -> String {
    use std::fmt::Write;
    key.bytes()
        .fold(String::with_capacity(key.len()), |mut out, b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                out.push(char::from(b));
            } else {
                let _ = write!(out, "%{b:02X}");
            }
            out
        })
}

fn is_scheme(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-'))
}

/// Rules R1 to R6: the normalized remote, or `None` for a local, `file:`, helper or unusable
/// remote, which is then ignored as input.
fn remote_key(url: &str) -> Option<String> {
    if url
        .get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case("file:"))
    {
        return None;
    }
    if url.find("::").is_some_and(|i| is_scheme(&url[..i])) {
        return None;
    }
    let (host, path) = match url.find("://") {
        Some(i) if is_scheme(&url[..i]) => {
            let rest = &url[i + 3..];
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
            let (host, after) = split_host(host)?;
            if !(after.is_empty()
                || after
                    .strip_prefix(':')
                    .is_some_and(|port| port.bytes().all(|b| b.is_ascii_digit())))
            {
                return None;
            }
            (host, path)
        }
        _ => {
            let colon = url.find(':')?;
            if url[..colon].contains('/') {
                return None;
            }
            let at = url[..colon].rfind('@').map_or(0, |i| i + 1);
            let (host, after) = split_host(&url[at..])?;
            (host, after.strip_prefix(':')?)
        }
    };
    let path = path.trim_matches('/');
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    (!host.is_empty() && !path.is_empty()).then(|| format!("{}/{path}", host.to_ascii_lowercase()))
}

/// Split a host from what follows it: a bracketed host runs through its `]`, any other up
/// to its first `:`. `None` for an unclosed bracket.
fn split_host(text: &str) -> Option<(&str, &str)> {
    if text.starts_with('[') {
        let end = text.find(']')? + 1;
        Some(text.split_at(end))
    } else {
        Some(text.find(':').map_or((text, ""), |i| text.split_at(i)))
    }
}

/// Rules P1 to P4: `<machine>:<path>`, the root relative to the dev root, strictly below it
/// and canonical.
fn path_key(machine: &Machine, root: &str, dev_root: &str) -> Option<String> {
    let dev = dev_root.trim_end_matches('/');
    let relative = root
        .strip_prefix(dev)?
        .strip_prefix('/')?
        .trim_end_matches('/');
    if relative.is_empty()
        || relative
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return None;
    }
    Some(format!("{}:{relative}", machine.as_str()))
}

/// The length and printable-ASCII checks every fact shares (ADR-0020 §3, rules 3 to 5).
fn printable(raw: &[u8]) -> Result<(), Flaw> {
    if raw.is_empty() {
        Err(Flaw::Empty)
    } else if raw.len() > MAX_FACT_BYTES {
        Err(Flaw::TooLong)
    } else if raw.iter().any(|b| !(0x21..=0x7e).contains(b)) {
        Err(Flaw::RawByte)
    } else {
        Ok(())
    }
}

/// Decode every `%HH`, in either hex case, exactly once.
fn decode(raw: &[u8]) -> Result<String, Flaw> {
    printable(raw)?;
    let mut out = Vec::with_capacity(raw.len());
    let mut rest = raw;
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%' {
            let hex = tail.get(..2).ok_or(Flaw::BadEscape)?;
            let pair = std::str::from_utf8(hex).map_err(|_| Flaw::BadEscape)?;
            if !hex.iter().all(u8::is_ascii_hexdigit) {
                return Err(Flaw::BadEscape);
            }
            out.push(u8::from_str_radix(pair, 16).map_err(|_| Flaw::BadEscape)?);
            rest = &tail[2..];
        } else {
            out.push(byte);
            rest = tail;
        }
    }
    let text = String::from_utf8(out).map_err(|_| Flaw::NotUtf8)?;
    if text.chars().any(char::is_control) {
        return Err(Flaw::Control);
    }
    Ok(text)
}

/// The dev root is sent as configured and never decoded: it must already be its own encoding.
fn dev_root(raw: &[u8]) -> Result<String, Flaw> {
    printable(raw)?;
    if raw.first() != Some(&b'/') || raw.iter().any(|b| matches!(b, b'%' | b'"' | b'\\')) {
        return Err(Flaw::NotPlainAbsolute);
    }
    Ok(String::from_utf8_lossy(raw).into_owned())
}

/// A scheme-form URL that still holds an `@` after stripping and gives no remote key had its
/// userinfo hidden from the strip: a `/` in the password, or the separating `@`, came encoded
/// (`https://user:pa/ss@…` once decoded). Judged on the decoded value alone, so every encoding
/// of one URL gives one outcome, and a dropped URL gave no key anyway: the flag never changes a
/// key.
fn unstripped_userinfo(url: &str) -> bool {
    url.find("://")
        .is_some_and(|at| is_scheme(&url[..at]) && url[at + 3..].contains('@'))
        && remote_key(url).is_none()
}

/// Remove the userinfo of every URL in free text, such as an error that quotes a tool argument.
///
/// At each `://`, the authority up to the next `/` (across whitespace, but not into the next
/// URL) loses everything through its last `@`. Then each whitespace-delimited token loses
/// everything through the last `@` before its first `:` in the scp form (no `/` before that
/// `:`), and through its last `@` when it is a scheme-form URL that rule 11 would drop (a `/`
/// in the password). Where a token's bounds are unclear, more is removed, never less.
/// ADR-0020 §3: nothing stored holds userinfo.
#[must_use]
pub fn redact_userinfo(text: &str) -> String {
    let mut schemed = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let start = at + 3;
        schemed.push_str(&rest[..start]);
        let tail = &rest[start..];
        // The authority runs to the next `/`, across whitespace (a secret may hold a space),
        // but never into the next URL's token.
        let next_url = tail.find("://").map_or(tail.len(), |at| {
            // The byte after the last whitespace char, which may be multibyte.
            match tail[..at]
                .trim_end_matches(|c: char| !c.is_whitespace())
                .len()
            {
                0 => at,
                token_start => token_start,
            }
        });
        let end = tail.find('/').unwrap_or(tail.len()).min(next_url);
        let authority = &tail[..end];
        schemed.push_str(
            authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host),
        );
        rest = &tail[end..];
    }
    schemed.push_str(rest);
    schemed
        .split_inclusive(char::is_whitespace)
        .map(|token| {
            let before = token.find(':').map_or("", |colon| &token[..colon]);
            match before.rfind('@') {
                Some(last) if !before.contains('/') => token[last + 1..].to_owned(),
                _ => redact_hidden_userinfo(token),
            }
        })
        .collect()
}

/// A token holding a scheme-form URL that rule 11 would drop loses everything between its
/// `://` and its last `@`.
fn redact_hidden_userinfo(token: &str) -> String {
    let Some(at) = token.find("://") else {
        return token.to_owned();
    };
    // Bytes, not chars: a non-ASCII byte is never a scheme byte, and the byte after one ends
    // its character, so the start is always a char boundary.
    let scheme_start = token.as_bytes()[..at]
        .iter()
        .rposition(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-')))
        .map_or(0, |i| i + 1);
    let url = token[scheme_start..].trim_end_matches(|c: char| {
        c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ',' | ';' | ')' | '\\')
    });
    match token.rfind('@') {
        Some(last) if last > at && unstripped_userinfo(url) => {
            format!("{}{}", &token[..at + 3], &token[last + 1..])
        }
        _ => token.to_owned(),
    }
}

/// Remove userinfo in two steps: the plugin's scheme rule (the authority after the first
/// `://`, up to the next `/`, loses everything through its last `@`), then its scp rule
/// (when a `:` exists and the text before the first one holds no `/`, that text loses
/// everything through its last `@`). It removes at least what the client removes.
fn strip_userinfo(url: &str) -> String {
    let mut out = url.to_owned();
    if let Some(at) = out.find("://") {
        let start = at + 3;
        let end = out[start..].find('/').map_or(out.len(), |i| start + i);
        if let Some(last) = out[start..end].rfind('@') {
            out.replace_range(start..=start + last, "");
        }
    }
    if let Some(colon) = out.find(':') {
        let before = &out[..colon];
        if !before.contains('/')
            && let Some(last) = before.rfind('@')
        {
            out.replace_range(..=last, "");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&str, &[u8])]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_bytes(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn bad_facts_carry_their_flaw() {
        for raw in [&b"a b"[..], b"a\tb", "caf\u{e9}".as_bytes()] {
            assert_eq!(decode(raw), Err(Flaw::RawByte), "{raw:?}");
        }
        for raw in ["%", "%4", "%G1", "a%z"] {
            assert_eq!(decode(raw.as_bytes()), Err(Flaw::BadEscape), "{raw}");
        }
        for raw in ["%C3", "%FF"] {
            assert_eq!(decode(raw.as_bytes()), Err(Flaw::NotUtf8), "{raw}");
        }
        for raw in ["%0A", "%00", "%7F", "%C2%85"] {
            assert_eq!(decode(raw.as_bytes()), Err(Flaw::Control), "{raw}");
        }
        assert_eq!(decode(&[b'a'; 4097]), Err(Flaw::TooLong));
        assert_eq!(decode(&[b'a'; 4096]), Ok("a".repeat(4096)));
        assert_eq!(decode(b""), Err(Flaw::Empty));
    }

    #[test]
    fn every_byte_and_every_escape_is_judged() {
        for b in 0..=u8::MAX {
            let kept = decode(&[b]).is_ok();
            assert_eq!(
                kept,
                (0x21..=0x7e).contains(&b) && b != b'%',
                "byte {b:#04x}"
            );
        }
        let hex = |b: u8| b.is_ascii_hexdigit();
        for x in 0..=u8::MAX {
            for y in 0..=u8::MAX {
                let raw = [b'%', x, y];
                let got = decode(&raw);
                let want = if !(0x21..=0x7e).contains(&x) || !(0x21..=0x7e).contains(&y) {
                    Err(Flaw::RawByte)
                } else if !hex(x) || !hex(y) {
                    Err(Flaw::BadEscape)
                } else {
                    let v = u8::from_str_radix(std::str::from_utf8(&[x, y]).unwrap(), 16).unwrap();
                    if (0x20..=0x7e).contains(&v) {
                        Ok(char::from(v).to_string())
                    } else if v < 0x80 {
                        Err(Flaw::Control)
                    } else {
                        Err(Flaw::NotUtf8)
                    }
                };
                assert_eq!(got, want, "%{x:02x}{y:02x}");
            }
        }
    }

    #[test]
    fn every_encoding_a_client_may_choose_decodes_alike() {
        let text = "café/x y\"\\%";
        let bytes = text.as_bytes();
        // Per byte: the spellings a client may send. A byte that must be encoded has two
        // (hex case); any other has three (raw, upper, lower).
        let choices: Vec<Vec<String>> = bytes
            .iter()
            .map(|&b| {
                let must = !(0x21..=0x7e).contains(&b) || matches!(b, b'%' | b'"' | b'\\');
                let mut c = vec![format!("%{b:02X}"), format!("%{b:02x}")];
                if !must {
                    c.push(char::from(b).to_string());
                }
                c
            })
            .collect();
        let mut index = vec![0_usize; choices.len()];
        let mut seen = 0_usize;
        loop {
            let encoded: String = index
                .iter()
                .zip(&choices)
                .map(|(&i, c)| c[i].as_str())
                .collect();
            assert_eq!(decode(encoded.as_bytes()).as_deref(), Ok(text), "{encoded}");
            seen += 1;
            let mut k = 0;
            loop {
                if k == index.len() {
                    assert_eq!(seen, 64 * 729);
                    return;
                }
                index[k] += 1;
                if index[k] < choices[k].len() {
                    break;
                }
                index[k] = 0;
                k += 1;
            }
        }
    }

    #[test]
    fn the_dev_root_is_its_own_encoding() {
        for kept in ["/home/alice/dev", "/home/alice/dev/"] {
            assert_eq!(dev_root(kept.as_bytes()).as_deref(), Ok(kept));
        }
        for bad in [
            "home/alice/dev",
            "/home/alice/d%65v",
            "/home/alice/\"dev\"",
            "/a\\b",
        ] {
            assert_eq!(
                dev_root(bad.as_bytes()),
                Err(Flaw::NotPlainAbsolute),
                "{bad}"
            );
        }
        assert_eq!(dev_root(b"/home/alice/my dev"), Err(Flaw::RawByte));
    }

    #[test]
    fn userinfo_is_stripped_from_every_remote_shape() {
        for (url, want) in [
            (
                "https://alice:s3cret@github.com/example/project.git",
                "https://github.com/example/project.git",
            ),
            (
                "https://TOKEN123@github.com/example/project",
                "https://github.com/example/project",
            ),
            (
                "https://alice:p@ss@github.com:8443/x",
                "https://github.com:8443/x",
            ),
            (
                "ssh://git@[2001:db8::1]:22/example/x.git",
                "ssh://[2001:db8::1]:22/example/x.git",
            ),
            (
                "git@github.com:example/project.git",
                "github.com:example/project.git",
            ),
            ("a@b@host:p", "host:p"),
            ("token@host:a://b", "host:a://b"),
            ("a/b@c://d@e/f", "a/b@c://e/f"),
            ("/srv/git/a@b.git", "/srv/git/a@b.git"),
            ("./a@b:c", "./a@b:c"),
            (
                "https://github.com/example/a@b",
                "https://github.com/example/a@b",
            ),
        ] {
            assert_eq!(strip_userinfo(url), want, "{url}");
        }
    }

    #[test]
    fn a_url_whose_userinfo_survives_stripping_is_dropped() {
        // A client that did not encode `%` sent a password's `/` (or the separating `@`)
        // encoded: decoded, the userinfo would survive the strip and give no remote key.
        for raw in [
            &b"https://alice:pa%2Fss@github.com/x"[..],
            b"https://alice:a%2Fb%2Fc@github.com/x",
            b"https://alice%40corp:pa%2Fss@github.com/x",
            b"https://alice:pa%2Fss%40github.com/x",
        ] {
            let map = headers(&[("ME-Project-Origin-Url", raw)]);
            let facts = ProjectFacts::from_headers(&map);
            assert_eq!(facts.origin_url, None, "{raw:?}");
            assert_eq!(
                facts.dropped.get("origin_url"),
                Some(&Flaw::UnstrippedUserinfo),
                "{raw:?}"
            );
        }
        // The judgement reads only the decoded value, so every encoding of one URL gives one
        // outcome (ADR-0020 §3: decoding is canonical).
        for (raw, stored) in [
            (
                &b"https://github.com/example/a@b"[..],
                "https://github.com/example/a@b",
            ),
            (
                b"https://github.com%2Fexample%2Fa@b",
                "https://github.com/example/a@b",
            ),
            (b"https://github.com/a@b/x", "https://github.com/a@b/x"),
            (
                b"https://alice:s3cret@github.com/example/a@b",
                "https://github.com/example/a@b",
            ),
            (b"git@host:a://b/c@d", "host:a://b/c@d"),
            (
                b"https://github.com:/example/a@b",
                "https://github.com:/example/a@b",
            ),
        ] {
            let map = headers(&[("ME-Project-Upstream-Url", raw)]);
            let facts = ProjectFacts::from_headers(&map);
            assert_eq!(facts.upstream_url.as_deref(), Some(stored), "{raw:?}");
        }
    }

    #[test]
    fn redaction_is_total_over_mixed_text() {
        // Every sequence of four pieces, non-ASCII ones next to schemes included, redacts
        // without a panic.
        let pieces = [
            "é", "😀", "\u{a0}", "\u{3000}", "a", ":", "/", "@", " ", "\"", "https://", "x:",
        ];
        for a in pieces {
            for b in pieces {
                for c in pieces {
                    for d in pieces {
                        let _ = redact_userinfo(&format!("{a}{b}{c}{d}"));
                    }
                }
            }
        }
    }

    #[test]
    fn text_loses_the_userinfo_of_every_url_it_holds() {
        for (text, want) in [
            (
                "unknown project: \"https://alice:s3cret@github.com/p\"",
                "unknown project: \"https://github.com/p\"",
            ),
            (
                "unknown variant `ssh://tok@host`, expected `project` or `user`",
                "unknown variant `ssh://host`, expected `project` or `user`",
            ),
            ("a://x@y b://z@w/q", "a://y b://w/q"),
            (
                "no url here, alice@example.org",
                "no url here, alice@example.org",
            ),
            (
                "unknown project: \"https://alice:p'ss@github.com/p\"",
                "unknown project: \"https://github.com/p\"",
            ),
            (
                "unknown project: \"TOKEN123@github.com:alice/fork.git\"",
                "unknown project: github.com:alice/fork.git\"",
            ),
            (
                "unknown variant `tok@host:x`, expected",
                "unknown variant host:x`, expected",
            ),
            (
                "unknown project: \"https://alice:p ss@github.com/p\"",
                "unknown project: \"https://github.com/p\"",
            ),
            (
                "unknown project: \"https://alice:pa/ss@github.com/p\"",
                "unknown project: \"https://github.com/p\"",
            ),
            (
                "unknown project: \"éhttps://a:b/c@d/e\"",
                "unknown project: \"éhttps://d/e\"",
            ),
            (
                "unknown variant `https://a\u{a0}https://b`, expected",
                "unknown variant `https://a\u{a0}https://b`, expected",
            ),
            (
                "https://github.com/example/a@b",
                "https://github.com/example/a@b",
            ),
        ] {
            assert_eq!(redact_userinfo(text), want, "{text}");
        }
    }

    #[test]
    fn headers_become_facts() {
        let map = headers(&[
            ("ME-Project-Root", b"/home/alice/dev/p"),
            ("ME-Project-Dev-Root", b"/home/alice/dev"),
            ("ME-Project-Upstream-Url", b"git@github.com:example/p.git"),
            (
                "ME-Project-Origin-Url",
                b"https://alice:x@github.com/alice/p",
            ),
            ("ME-Project-Branch", b"feat%2Fx"),
        ]);
        let facts = ProjectFacts::from_headers(&map);
        assert_eq!(facts.root.as_deref(), Some("/home/alice/dev/p"));
        assert_eq!(facts.dev_root.as_deref(), Some("/home/alice/dev"));
        assert_eq!(
            facts.upstream_url.as_deref(),
            Some("github.com:example/p.git")
        );
        assert_eq!(
            facts.origin_url.as_deref(),
            Some("https://github.com/alice/p")
        );
        assert_eq!(facts.branch.as_deref(), Some("feat/x"));
        assert!(facts.dropped.is_empty());

        let map = headers(&[
            ("ME-Project-Root", b"/home/alice/dev/p"),
            ("ME-Project-Origin-Url", b"https://github.com/a/p"),
            ("ME-Project-Origin-Url", b"https://github.com/b/p"),
        ]);
        let facts = ProjectFacts::from_headers(&map);
        assert_eq!(facts.origin_url, None);
        assert_eq!(facts.dropped.get("origin_url"), Some(&Flaw::Repeated));
        assert_eq!(facts.root.as_deref(), Some("/home/alice/dev/p"));
        assert_eq!(facts.dev_root, None);
        assert!(!facts.dropped.contains_key("dev_root"));
    }

    fn laptop() -> crate::auth::Machine {
        crate::auth::Machine::named("laptop")
    }

    fn remote(url: &str) -> Option<String> {
        ProjectFacts {
            origin_url: Some(url.to_owned()),
            ..ProjectFacts::default()
        }
        .key(&laptop())
        .map(|k| k.as_str().to_owned())
    }

    fn path(root: &str, dev: &str) -> Option<String> {
        ProjectFacts {
            root: Some(root.to_owned()),
            dev_root: Some(dev.to_owned()),
            ..ProjectFacts::default()
        }
        .key(&laptop())
        .map(|k| k.as_str().to_owned())
    }

    #[test]
    fn remote_keys_follow_rules_v1() {
        for url in [
            "git@github.com:example/project.git",
            "https://github.com/example/project.git/",
            "ssh://git@GitHub.com:22/example/project.git",
            "git+ssh://git@github.com/example/project",
            "https://alice:s3cret@github.com/example/project",
        ] {
            assert_eq!(
                remote(&strip_userinfo(url)).as_deref(),
                Some("github.com/example/project"),
                "{url}"
            );
        }
        for (url, key) in [
            (
                "https://github.com/example/Project",
                "github.com/example/Project",
            ),
            (
                "ssh://git@[2001:DB8::1]:2222/example/x.git",
                "[2001:db8::1]/example/x",
            ),
            ("git@[2001:db8::1]:example/x", "[2001:db8::1]/example/x"),
            ("https://github.com/example/a:b", "github.com/example/a:b"),
            ("git@github.com:/srv/x.git", "github.com/srv/x"),
            ("ssh://github.com/srv/x", "github.com/srv/x"),
            (
                "https://github.com/example/X.GIT",
                "github.com/example/X.GIT",
            ),
        ] {
            assert_eq!(remote(&strip_userinfo(url)).as_deref(), Some(key), "{url}");
        }
        for url in [
            "file:///srv/x.git",
            "FILE:/x",
            "/srv/x.git",
            "./x",
            "../x",
            "x",
            "./a:b",
            "hg::ssh://host/x",
            "https://github.com",
            "https://github.com/",
            "https:///x",
            "https://host/.git",
            "https://github.com:abc/x",
            "ssh://[2001:db8::1/x",
            "git@:x",
            "host:",
            ":x",
        ] {
            assert_eq!(remote(&strip_userinfo(url)), None, "{url}");
        }
    }

    #[test]
    fn the_key_order_is_upstream_origin_path() {
        let facts = |upstream: Option<&str>, origin: Option<&str>| ProjectFacts {
            root: Some("/home/alice/dev/p".to_owned()),
            dev_root: Some("/home/alice/dev".to_owned()),
            upstream_url: upstream.map(str::to_owned),
            origin_url: origin.map(str::to_owned),
            ..ProjectFacts::default()
        };
        let key = |f: ProjectFacts| f.key(&laptop()).map(|k| k.as_str().to_owned());
        let up = Some("github.com:example/p.git");
        let fork = Some("https://github.com/alice/p");
        assert_eq!(
            key(facts(up, fork)).as_deref(),
            Some("github.com/example/p")
        );
        assert_eq!(
            key(facts(Some("/srv/p.git"), fork)).as_deref(),
            Some("github.com/alice/p")
        );
        assert_eq!(
            key(facts(None, fork)).as_deref(),
            Some("github.com/alice/p")
        );
        assert_eq!(
            key(facts(Some("file:///p"), Some("/srv/p"))).as_deref(),
            Some("laptop:p")
        );
        let mut none = facts(None, None);
        none.root = None;
        assert_eq!(key(none), None);
    }

    #[test]
    fn path_keys_follow_rules_v1() {
        for dev in ["/home/alice/perso/dev", "/home/alice/perso/dev/"] {
            for root in [
                "/home/alice/perso/dev/notes/2026",
                "/home/alice/perso/dev/notes/2026/",
            ] {
                assert_eq!(
                    path(root, dev).as_deref(),
                    Some("laptop:notes/2026"),
                    "{root} {dev}"
                );
            }
        }
        let dev = "/home/alice/perso/dev";
        assert_eq!(
            path("/home/alice/perso/dev/café", dev).as_deref(),
            Some("laptop:café")
        );
        assert_eq!(
            path("/home/alice/perso/dev/a:b", dev).as_deref(),
            Some("laptop:a:b")
        );
        assert_eq!(
            path("/home/alice/x", "/").as_deref(),
            Some("laptop:home/alice/x")
        );
        for root in [
            "/home/alice/perso/dev",
            "/home/alice/perso/dev/",
            "/home/alice/perso/devtools/x",
            "/home/alice/x",
            "/home/alice/perso/dev//x",
            "/home/alice/perso/dev/./x",
            "/home/alice/perso/dev/a/../b",
            "notes/2026",
        ] {
            assert_eq!(path(root, dev), None, "{root}");
        }
        assert_eq!(path("x", "/"), None);
    }

    #[test]
    fn the_segment_limit_rejects_the_final_key() {
        let dev = "/home/alice/dev";
        let rel = |s: String| path(&format!("{dev}/{s}"), dev);
        assert!(rel("a".repeat(239)).is_some());
        assert!(rel("a".repeat(240)).is_none());
        assert!(rel("é".repeat(39)).is_some());
        assert!(rel("é".repeat(40)).is_none());
        let long = ProjectFacts {
            upstream_url: Some(format!("https://github.com/example/{}", "a".repeat(240))),
            origin_url: Some("https://github.com/example/p".to_owned()),
            ..ProjectFacts::default()
        };
        assert_eq!(long.key(&laptop()), None);
        assert_eq!(ProjectKey::new(""), None);
    }

    #[test]
    fn path_keys_and_remote_keys_never_collide() {
        let shadow = path(
            "/home/alice/dev/github.com/example/project",
            "/home/alice/dev",
        );
        assert_eq!(shadow.as_deref(), Some("laptop:github.com/example/project"));
        let keys = [
            "github.com/example/project",
            "github.com/example/a:b",
            "[2001:db8::1]/example/x",
            "laptop:a:b",
            "laptop:github.com/example/project",
            "laptop:notes/2026",
        ];
        for key in keys {
            let colon = key.find(':');
            let slash = key.find('/');
            // ADR-0020 §7: a path key starts with a machine name (a letter or a digit) and
            // has no `/` before its first `:`.
            let is_path = key.starts_with(|c: char| c.is_ascii_alphanumeric())
                && colon.is_some_and(|c| slash.is_none_or(|s| c < s));
            let is_remote =
                key.starts_with('[') || colon.is_none_or(|c| slash.is_some_and(|s| s < c));
            assert!(is_path != is_remote, "{key}");
        }
        let labels: std::collections::HashSet<_> = keys
            .iter()
            .map(|k| ProjectKey::new(k).unwrap().label().to_owned())
            .collect();
        assert_eq!(labels.len(), keys.len());
    }

    #[test]
    fn labels_follow_the_adr_examples() {
        assert_eq!(
            ProjectKey::new("github.com/example/project")
                .unwrap()
                .label(),
            "github.com%2Fexample%2Fproject"
        );
        assert_eq!(
            ProjectKey::new("laptop:notes/2026").unwrap().label(),
            "laptop%3Anotes%2F2026"
        );
    }

    #[test]
    fn labels_encode_every_other_byte() {
        let label = |k: &str| ProjectKey::new(k).unwrap().label().to_owned();
        assert_eq!(label("%"), "%25");
        assert_eq!(label("é"), "%C3%A9");
        assert_eq!(label("a~b-c.d_e"), "a~b-c.d_e");
        assert_ne!(label("a/b"), label("a%2Fb"));
    }
}
