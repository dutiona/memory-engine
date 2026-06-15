//! Fact extraction from candidate episodes.
//!
//! Provides the [`SessionExtractor`] trait and a default [`KeywordExtractor`]
//! implementation that maps category + outcome to fact type and importance
//! without requiring an LLM.

use crate::error::Result;
use crate::types::FactType;

use super::filter::{CandidateEpisode, EpisodeCategory};
use super::outcome::SessionOutcome;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A fact extracted from a session episode.
#[derive(Debug, Clone)]
pub struct ExtractedFact {
    pub content: String,
    pub fact_type: FactType,
    pub importance: f64,
    pub category: EpisodeCategory,
    pub metadata: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Trait for extracting facts from candidate episodes.
///
/// The default implementation ([`KeywordExtractor`]) uses the keyword
/// pre-filter output directly. Consumers can implement this with an LLM to
/// produce higher-quality, parameterized procedural patterns.
pub trait SessionExtractor {
    /// Extract facts from a candidate episode.
    ///
    /// # Errors
    ///
    /// Returns an error if extraction fails (e.g., LLM call failure).
    fn extract(
        &self,
        episode: &CandidateEpisode,
        outcome: &SessionOutcome,
    ) -> Result<Vec<ExtractedFact>>;
}

// ---------------------------------------------------------------------------
// KeywordExtractor
// ---------------------------------------------------------------------------

/// Default keyword-based extractor. No LLM required.
///
/// Maps `(EpisodeCategory, SessionOutcome)` pairs to `(FactType, importance)`
/// and concatenates turn text into a single content string (truncated to 2000
/// characters).
pub struct KeywordExtractor;

impl SessionExtractor for KeywordExtractor {
    fn extract(
        &self,
        episode: &CandidateEpisode,
        outcome: &SessionOutcome,
    ) -> Result<Vec<ExtractedFact>> {
        let content = build_content(episode);

        let (fact_type, importance) = match (&episode.category, outcome) {
            (EpisodeCategory::Bug, SessionOutcome::Success) => (FactType::Procedural, 0.7),
            (EpisodeCategory::Bug, SessionOutcome::Failure) => (FactType::Episodic, 0.4),
            (EpisodeCategory::Bug, SessionOutcome::Indeterminate) => (FactType::Episodic, 0.5),
            (EpisodeCategory::Decision | EpisodeCategory::Learning, _) => (FactType::Semantic, 0.6),
            (EpisodeCategory::Convention, _) => (FactType::Procedural, 0.8),
        };

        let mut metadata = serde_json::json!({
            "session_id": episode.session_id,
            "category": format!("{:?}", episode.category),
            "matched_keywords": episode.matched_keywords,
        });

        match outcome {
            SessionOutcome::Failure => metadata["session_outcome"] = "failure".into(),
            SessionOutcome::Success => metadata["session_outcome"] = "success".into(),
            SessionOutcome::Indeterminate => {}
        }

        Ok(vec![ExtractedFact {
            content,
            fact_type,
            importance,
            category: episode.category,
            metadata,
        }])
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build content string from episode turns, truncated to `MAX_LEN` chars.
fn build_content(episode: &CandidateEpisode) -> String {
    const MAX_LEN: usize = 2000;

    let mut parts = Vec::new();
    for turn in &episode.turns {
        if !turn.user_text.is_empty() {
            parts.push(format!("User: {}", turn.user_text));
        }
        if !turn.assistant_text.is_empty() {
            parts.push(format!("Assistant: {}", turn.assistant_text));
        }
    }
    let full = parts.join("\n");

    if full.len() > MAX_LEN {
        let mut end = MAX_LEN;
        // Don't cut in the middle of a multi-byte char.
        while !full.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}...", &full[..end])
    } else {
        full
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use super::super::filter::ConversationTurn;

    fn make_episode(
        category: EpisodeCategory,
        user_text: &str,
        assistant_text: &str,
    ) -> CandidateEpisode {
        CandidateEpisode {
            category,
            turns: vec![ConversationTurn {
                timestamp: Utc::now(),
                user_text: user_text.into(),
                assistant_text: assistant_text.into(),
                tool_calls: vec![],
                uuid: "test".into(),
            }],
            matched_keywords: vec!["test".into()],
            session_id: "sess-1".into(),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn keyword_extractor_bug_success() {
        let ep = make_episode(EpisodeCategory::Bug, "fix this", "done");
        let ext = KeywordExtractor;
        let facts = ext.extract(&ep, &SessionOutcome::Success).unwrap();

        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].fact_type, FactType::Procedural);
        assert!((facts[0].importance - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn keyword_extractor_bug_failure() {
        let ep = make_episode(EpisodeCategory::Bug, "fix this", "failed");
        let ext = KeywordExtractor;
        let facts = ext.extract(&ep, &SessionOutcome::Failure).unwrap();

        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].fact_type, FactType::Episodic);
        assert!((facts[0].importance - 0.4).abs() < f64::EPSILON);
        assert_eq!(facts[0].metadata["session_outcome"], "failure");
    }

    #[test]
    fn keyword_extractor_decision() {
        let ep = make_episode(EpisodeCategory::Decision, "use tokio?", "yes");
        let ext = KeywordExtractor;
        let facts = ext.extract(&ep, &SessionOutcome::Indeterminate).unwrap();

        assert_eq!(facts[0].fact_type, FactType::Semantic);
        assert!((facts[0].importance - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn keyword_extractor_convention() {
        let ep = make_episode(EpisodeCategory::Convention, "use snake_case", "ok");
        let ext = KeywordExtractor;
        let facts = ext.extract(&ep, &SessionOutcome::Success).unwrap();

        assert_eq!(facts[0].fact_type, FactType::Procedural);
        assert!((facts[0].importance - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn keyword_extractor_learning() {
        let ep = make_episode(EpisodeCategory::Learning, "TIL about lifetimes", "neat");
        let ext = KeywordExtractor;
        let facts = ext.extract(&ep, &SessionOutcome::Success).unwrap();

        assert_eq!(facts[0].fact_type, FactType::Semantic);
        assert!((facts[0].importance - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn content_truncation() {
        let long_text = "a".repeat(3000);
        let ep = make_episode(EpisodeCategory::Bug, &long_text, "");
        let ext = KeywordExtractor;
        let facts = ext.extract(&ep, &SessionOutcome::Success).unwrap();

        // "User: " prefix is 6 chars, so total > 2000
        assert!(facts[0].content.ends_with("..."));
        // The implementation truncates to MAX_LEN=2000 chars then appends "...",
        // so the total length must be exactly MAX_LEN + 3 = 2003 at most.
        assert!(facts[0].content.len() <= 2003);
    }

    #[test]
    fn custom_extractor() {
        /// A mock extractor that always returns an empty vec.
        struct NullExtractor;

        impl SessionExtractor for NullExtractor {
            fn extract(
                &self,
                _episode: &CandidateEpisode,
                _outcome: &SessionOutcome,
            ) -> Result<Vec<ExtractedFact>> {
                Ok(vec![])
            }
        }

        let ep = make_episode(EpisodeCategory::Bug, "x", "y");
        let ext: Box<dyn SessionExtractor> = Box::new(NullExtractor);
        let facts = ext.extract(&ep, &SessionOutcome::Success).unwrap();
        assert!(facts.is_empty());
    }
}
