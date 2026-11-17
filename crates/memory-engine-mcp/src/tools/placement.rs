//! Placement receipts and the doubtful-placement hint (ADR-0020 §8, ADR-0021 §7).
//!
//! The hint is deterministic (ADR-0004: no LLM in core), advisory and outside the contract:
//! it never refuses and never moves a memory, so its phrase lists can change freely.

use crate::home::Home;

/// Phrases that read as true in every project.
const USER_WIDE: [&str; 10] = [
    "every project",
    "all projects",
    "all my projects",
    "any project",
    "across projects",
    "i prefer",
    "my preference",
    "the user prefers",
    "the user always",
    "the user never",
];

/// Phrases that read as specific to one project.
const PROJECT_SPECIFIC: [&str; 6] = [
    "this project",
    "this repo",
    "this repository",
    "this codebase",
    "this crate",
    "this checkout",
];

/// Where a memory now is, and who sees it.
pub fn receipt(id: i64, home: &Home) -> String {
    format!("stored {id} in {}, visible to all agents", home.words())
}

/// The answer to a `set_level` whose memory is already at that level.
pub fn unchanged(id: i64, home: &Home) -> String {
    format!(
        "unchanged: {id} is already in {}, visible to all agents",
        home.words()
    )
}

/// One hint line naming `set_level` when a memory filed at `target` reads as if it belonged
/// at the other level; `own` is the session's own Home, which a project-level move targets.
pub fn hint(content: &str, id: i64, target: &Home, own: &Home) -> Option<String> {
    let text = content.to_ascii_lowercase();
    let says = |phrases: &[&str]| phrases.iter().any(|p| holds_phrase(&text, p));
    match target {
        Home::User if matches!(own, Home::Project(_)) && says(&PROJECT_SPECIFIC) => Some(format!(
            "hint: this reads as specific to one project; set_level({id}, \"project\") files it in {}",
            own.words()
        )),
        Home::Project(_) | Home::Bucket(_) if says(&USER_WIDE) => Some(format!(
            "hint: this reads as true in every project; set_level({id}, \"user\") files it at the user level"
        )),
        _ => None,
    }
}

/// `phrase` occurs in `text` with a non-alphanumeric character or an edge on each side.
fn holds_phrase(text: &str, phrase: &str) -> bool {
    text.match_indices(phrase).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + phrase.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Machine;
    use crate::project::ProjectKey;

    fn project() -> Home {
        Home::Project(ProjectKey::new("github.com/example/p").unwrap())
    }

    #[test]
    fn hints_fire_on_whole_phrases_only() {
        for phrase in USER_WIDE {
            let text = format!("Note: {phrase}, always.");
            assert!(hint(&text, 1, &project(), &project()).is_some(), "{phrase}");
            assert!(
                hint(
                    &text,
                    1,
                    &Home::Bucket(Machine::named("laptop")),
                    &project()
                )
                .is_some(),
                "{phrase}"
            );
        }
        for phrase in PROJECT_SPECIFIC {
            let text = format!("In {phrase}: x.");
            assert!(
                hint(&text, 1, &Home::User, &project()).is_some(),
                "{phrase}"
            );
        }
        assert!(hint("This repository pins it", 1, &Home::User, &project()).is_some());
        for text in ["wifi prefers 5 GHz", "this repos list", "everyproject"] {
            assert!(hint(text, 1, &project(), &project()).is_none(), "{text}");
            assert!(hint(text, 1, &Home::User, &project()).is_none(), "{text}");
        }
        assert!(hint("i prefer tabs", 1, &Home::User, &project()).is_none());
        assert!(hint("this repo is odd", 1, &project(), &project()).is_none());
        assert!(
            hint(
                "this repo is odd",
                1,
                &Home::User,
                &Home::Bucket(Machine::named("laptop"))
            )
            .is_none(),
            "a keyless session has no project to suggest"
        );
    }

    #[test]
    fn receipts_name_the_home() {
        assert_eq!(
            receipt(7, &Home::User),
            "stored 7 in user, visible to all agents"
        );
        assert_eq!(
            receipt(7, &project()),
            "stored 7 in project github.com/example/p, visible to all agents"
        );
        assert_eq!(
            receipt(7, &Home::Bucket(Machine::named("laptop"))),
            "stored 7 in unscoped on laptop, visible to all agents"
        );
    }
}
