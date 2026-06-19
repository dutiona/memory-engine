//! Dialect-free backend capability signals — how "is lexical retrieval degraded?"
//! surfaces to the engine and the retrieval-quality benchmark **without** leaking
//! whether the backend uses `bm25()` vs `ts_rank_cd`.

/// Which lexical ranking algorithm a backend's `SearchIndex` uses — the
/// dialect-free signal behind the BM25-vs-`ts_rank` tier.
///
/// `Bm25` ⇒ true corpus IDF + TF saturation; `TsRankCd` ⇒ frequency/position
/// weighting with no global statistics (degraded recall on rare-term queries,
/// bounded by RRF + the vector channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LexicalRanker {
    /// Okapi BM25 — `SQLite` FTS5 `bm25()`, or Postgres `pg_search`/`pg_textsearch`.
    Bm25,
    /// Postgres `ts_rank_cd` — cover-density, no IDF. The "stock managed PG" tier.
    TsRankCd,
}

/// What a backend can do, surfaced *without leaking dialect*. Consumed by the
/// engine and the piece-D retrieval-quality benchmark; each field is a property a
/// tier either has or does not.
///
/// Returned by `SchemaManager::capabilities`, which is **synchronous** —
/// capabilities are fixed at open, not a per-call round-trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BackendCapabilities {
    /// The lexical ranker in effect — the "is retrieval degraded?" signal.
    pub lexical_ranker: LexicalRanker,
    /// Whether vector search runs server-side (`pgvector`) vs in-process (`SQLite`
    /// brute-force / in-memory HNSW).
    pub server_side_vector: bool,
    /// Whether the lexical ranker uses true corpus IDF (`Bm25` ⇒ `true`,
    /// `TsRankCd` ⇒ `false`). Redundant with `lexical_ranker` today, kept explicit
    /// so the benchmark reports it without re-deriving.
    pub true_idf: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_ranker_is_copy_and_eq() {
        let a = LexicalRanker::Bm25;
        let b = a; // Copy
        assert_eq!(a, b);
        assert_ne!(LexicalRanker::Bm25, LexicalRanker::TsRankCd);
    }

    #[test]
    fn capabilities_construct_both_tiers() {
        let bm25 = BackendCapabilities {
            lexical_ranker: LexicalRanker::Bm25,
            server_side_vector: false,
            true_idf: true,
        };
        let stock = BackendCapabilities {
            lexical_ranker: LexicalRanker::TsRankCd,
            server_side_vector: true,
            true_idf: false,
        };
        assert_ne!(bm25, stock);
        assert_eq!(bm25.lexical_ranker, LexicalRanker::Bm25);
        assert!(bm25.true_idf && !bm25.server_side_vector);
        assert!(stock.server_side_vector && !stock.true_idf);
    }
}
