//! CLI-local fact-type plumbing.
//!
//! The canonical string→[`FactType`] mapping lives in **core** as
//! [`FactType::from_str`] (case-insensitive, accepts both `snake_case` and
//! `PascalCase`) — it is the single source of truth shared with the MCP server
//! (#678). This module holds only the two CLI-framework shims that cannot live in
//! the framework-free core crate:
//!
//! * [`CliFactType`] — a `clap::ValueEnum` so `--fact-type` gets `[possible
//!   values: …]` in `--help` and shell completion. Its tokens are locked to
//!   core's canonical casing by a round-trip test below, so they cannot drift.
//! * [`deserialize_fact_type`] — a serde adapter that parses the JSONL
//!   `fact_type` field through core's `FromStr`, so batch-ingest shares the exact
//!   same parser as every other surface.

use std::str::FromStr;

use memory_engine::types::FactType;
use serde::Deserialize;

/// Fact type as accepted on the CLI (`--fact-type` on `add-fact` / `query`).
///
/// This exists solely to give clap a `ValueEnum` (core stays framework-free). The
/// value tokens are the lower-cased variant names (`episodic`, `semantic`,
/// `procedural`), matching core's canonical [`FactType`] `Display`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum CliFactType {
    Episodic,
    Semantic,
    Procedural,
}

impl From<CliFactType> for FactType {
    fn from(t: CliFactType) -> Self {
        match t {
            CliFactType::Episodic => Self::Episodic,
            CliFactType::Semantic => Self::Semantic,
            CliFactType::Procedural => Self::Procedural,
        }
    }
}

/// Serde adapter for the JSONL `fact_type` field, routing through core's
/// canonical [`FactType::from_str`].
///
/// Use via `#[serde(deserialize_with = "crate::commands::types::deserialize_fact_type")]`.
/// Because it delegates to core, the CLI's JSONL ingest accepts the same casings
/// as every other surface (`snake_case` and `PascalCase`) and can never diverge.
pub fn deserialize_fact_type<'de, D>(deserializer: D) -> Result<FactType, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    FactType::from_str(&s).map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_to_engine_fact_type() {
        assert_eq!(FactType::from(CliFactType::Episodic), FactType::Episodic);
        assert_eq!(FactType::from(CliFactType::Semantic), FactType::Semantic);
        assert_eq!(
            FactType::from(CliFactType::Procedural),
            FactType::Procedural
        );
    }

    /// The JSONL adapter delegates to core, so it accepts the canonical
    /// `snake_case` and rejects unknown tokens.
    #[test]
    fn deserialize_fact_type_parses_snake_case() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(deserialize_with = "deserialize_fact_type")]
            fact_type: FactType,
        }

        let w: Wrapper = serde_json::from_str(r#"{"fact_type":"episodic"}"#).unwrap();
        assert_eq!(w.fact_type, FactType::Episodic);
        assert!(serde_json::from_str::<Wrapper>(r#"{"fact_type":"unknown"}"#).is_err());
    }

    #[test]
    fn clap_value_tokens_are_lowercase() {
        use clap::ValueEnum;
        let tokens: Vec<_> = CliFactType::value_variants()
            .iter()
            .map(|v| v.to_possible_value().unwrap().get_name().to_owned())
            .collect();
        assert_eq!(tokens, ["episodic", "semantic", "procedural"]);
    }

    /// Structural lock: every clap token must parse through core's canonical
    /// `FromStr` to the same `FactType` the clap `From` conversion yields, and
    /// core's `Display` must reproduce that token. This makes the clap surface
    /// unable to silently drift from the core single source of truth (#678).
    #[test]
    fn clap_tokens_round_trip_through_core_from_str() {
        use clap::ValueEnum;
        for &variant in CliFactType::value_variants() {
            let token = variant.to_possible_value().unwrap().get_name().to_owned();
            let parsed: FactType = token.parse().expect("clap token must parse via core");
            assert_eq!(parsed, FactType::from(variant));
            assert_eq!(parsed.to_string(), token);
        }
    }
}
