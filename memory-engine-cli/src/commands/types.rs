//! CLI-local fact-type wrapper shared across subcommands.
//!
//! `memory_engine::FactType` deliberately exposes no `clap::ValueEnum`,
//! `serde::Deserialize`, or `FromStr` impl (the core crate has no CLI/serde
//! concerns), so the CLI needs a thin local wrapper. This is the single source of
//! truth for it: `add-fact` (clap arg), `query` (clap arg), and `batch-ingest`
//! (JSONL field) all parse into [`CliFactType`] and convert via `From`.

use memory_engine::types::FactType;

/// Fact type as accepted on the CLI (`--fact-type`) and in JSONL (`fact_type`).
///
/// The clap value tokens and the serde field values are both the lower-cased
/// variant names (`episodic`, `semantic`, `procedural`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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

    #[test]
    fn deserializes_snake_case() {
        assert_eq!(
            serde_json::from_str::<CliFactType>("\"episodic\"").unwrap(),
            CliFactType::Episodic
        );
        assert!(serde_json::from_str::<CliFactType>("\"unknown\"").is_err());
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
}
