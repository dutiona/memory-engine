//! Homes: where a memory is filed and what a session reads (ADR-0020 §7, ADR-0021 §3).

use std::fmt;

use crate::auth::Machine;
use crate::project::ProjectKey;

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

/// A Home: the one scope node a memory is filed at (ADR-0021 §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Home {
    /// The user level, `user:<user>`.
    User,
    /// A project, `user:<user>/project:<label>`.
    Project(ProjectKey),
    /// A machine's bucket, `unscoped:<machine>`, a top-level node no query rooted at the
    /// user reaches.
    Bucket(Machine),
}

impl Home {
    /// The Home's scope path.
    pub(crate) fn path(&self, user: &User) -> String {
        match self {
            Self::User => format!("user:{user}"),
            Self::Project(key) => format!("user:{user}/project:{}", key.label()),
            Self::Bucket(machine) => format!("unscoped:{}", machine.as_str()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alice() -> User {
        User::parse("alice").unwrap()
    }

    #[test]
    fn homes_render_their_paths() {
        let key = ProjectKey::new("github.com/example/project").unwrap();
        assert_eq!(Home::User.path(&alice()), "user:alice");
        assert_eq!(
            Home::Project(key).path(&alice()),
            "user:alice/project:github.com%2Fexample%2Fproject"
        );
        assert_eq!(
            Home::Bucket(Machine::named("laptop")).path(&alice()),
            "unscoped:laptop"
        );
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
