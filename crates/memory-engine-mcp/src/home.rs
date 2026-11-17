//! Homes: where a memory is filed and what a session reads (ADR-0020 §7, ADR-0021 §3).
//!
//! Until #1022 derives a project key from each request, every session is keyless: it files
//! into its machine's bucket and reads that bucket and the user level (ADR-0021 §6).

use std::fmt;

use memory_engine::types::MAX_SEGMENT_LEN;

use crate::auth::Machine;

/// The owner, `user:<user>` in the scope tree: a server setting (ADR-0020 §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User(String);

impl User {
    /// Accept a lowercase DNS label, the machine-name grammar of ADR-0020 §4, so that
    /// `user:<user>` is always one valid scope segment.
    ///
    /// # Errors
    ///
    /// The reason `value` is not a user name.
    pub fn parse(value: &str) -> Result<Self, String> {
        if crate::auth::is_dns_label(value.as_bytes()) {
            Ok(Self(value.to_owned()))
        } else {
            Err(format!(
                "a user is a lowercase DNS label, [a-z0-9]([a-z0-9-]{{0,61}}[a-z0-9])?: {value:?}"
            ))
        }
    }
}

impl fmt::Display for User {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The user level.
pub(crate) fn user_home(user: &User) -> String {
    format!("user:{user}")
}

/// A machine's bucket, a top-level node no query rooted at the user reaches.
pub(crate) fn bucket(machine: &Machine) -> String {
    format!("unscoped:{}", machine.as_str())
}

/// The project whose key is `key`, or `None` when its segment exceeds the scope tree's limit
/// (ADR-0020 §7: then there is no key).
pub(crate) fn project_home(user: &User, key: &str) -> Option<String> {
    let segment = format!("project:{}", label(key));
    (segment.len() <= MAX_SEGMENT_LEN).then(|| format!("{}/{segment}", user_home(user)))
}

/// ADR-0020 §7: every byte outside `A-Z a-z 0-9 - . _ ~` percent-encoded in uppercase hex.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn alice() -> User {
        User::parse("alice").unwrap()
    }

    #[test]
    fn labels_follow_the_adr_examples() {
        assert_eq!(
            label("github.com/example/project"),
            "github.com%2Fexample%2Fproject"
        );
        assert_eq!(label("laptop:notes/2026"), "laptop%3Anotes%2F2026");
    }

    #[test]
    fn labels_encode_every_other_byte() {
        assert_eq!(label("%"), "%25");
        assert_eq!(label("é"), "%C3%A9");
        assert_eq!(label("a~b-c.d_e"), "a~b-c.d_e");
        assert_ne!(label("a/b"), label("a%2Fb"));
    }

    #[test]
    fn a_project_home_sits_under_the_user() {
        assert_eq!(
            project_home(&alice(), "github.com/example/project").as_deref(),
            Some("user:alice/project:github.com%2Fexample%2Fproject")
        );
        assert_eq!(user_home(&alice()), "user:alice");
    }

    #[test]
    fn a_segment_past_the_scope_limit_has_no_home() {
        let fits = "a".repeat(MAX_SEGMENT_LEN - "project:".len());
        assert!(project_home(&alice(), &fits).is_some());
        assert!(project_home(&alice(), &format!("{fits}a")).is_none());
        // Each `/` costs three bytes once encoded.
        assert!(project_home(&alice(), &"/".repeat(83)).is_none());
    }

    #[test]
    fn a_user_is_a_lowercase_dns_label() {
        assert!(User::parse("alice").is_ok());
        assert!(User::parse(&"a".repeat(63)).is_ok());
        for bad in ["", "Alice", "a/b", "-a", "a-", &"a".repeat(64)] {
            assert!(User::parse(bad).is_err(), "{bad:?}");
        }
    }
}
