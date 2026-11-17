//! The request's project facts (ADR-0020 §3).
//!
//! The five `ME-Project-*` headers are decoded once, canonically: a bad fact is dropped and
//! flagged, never the request. Both remote URLs lose their userinfo before anything holds
//! them. Nothing here depends on MCP, so the lifecycle routes (#1025) decode the same way.

use std::collections::BTreeMap;

use axum::http::HeaderMap;
use serde::Serialize;

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
                Ok(value) => {
                    let slot = facts.slot(name);
                    *slot = Some(if name.ends_with("_url") {
                        strip_userinfo(&value)
                    } else {
                        value
                    });
                }
                Err(flaw) => {
                    facts.dropped.insert(name, flaw);
                }
            }
        }
        facts
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
}
