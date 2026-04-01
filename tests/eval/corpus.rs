//! Golden corpus for the evaluation harness.
//!
//! ~50 facts across 3 domains + cross-cutting concerns, with 25 queries
//! and graded relevance judgments. Designed for blake3 deterministic
//! embeddings where FTS5 carries retrieval — queries share keywords
//! with relevant facts.

use chrono::{DateTime, Duration, Utc};
use memory_engine::types::FactType;

// ---------------------------------------------------------------------------
// Corpus types
// ---------------------------------------------------------------------------

/// Options for corpus fact insertion.
#[derive(Clone, Debug)]
pub struct FactOpts {
    pub importance: Option<f64>,
    pub pinned: Option<bool>,
    pub t_valid: Option<DateTime<Utc>>,
    pub t_invalid: Option<DateTime<Utc>>,
    pub t_created: Option<DateTime<Utc>>,
    pub last_accessed: Option<DateTime<Utc>>,
}

/// A fact in the golden corpus.
#[derive(Clone, Debug)]
pub struct CorpusFact {
    pub content: &'static str,
    pub fact_type: FactType,
    pub scope: Option<&'static str>,
    pub opts: Option<FactOpts>,
}

/// A query with graded relevance judgments.
#[derive(Clone, Debug)]
pub struct CorpusQuery {
    /// Query text (searched via FTS + vector).
    pub text: &'static str,
    /// `(fact_index, relevance_grade)` — grade: 0=irrelevant, 1=marginal,
    /// 2=relevant, 3=highly relevant.
    pub relevance: &'static [(usize, u32)],
    /// Optional scope filter for the query.
    pub scope: Option<&'static str>,
    /// Whether this query should assert hybrid participation (vector_candidates > 0).
    pub assert_hybrid: bool,
    /// Human-readable description of what this query tests.
    pub description: &'static str,
}

/// Complete corpus definition.
pub struct CorpusDefinition {
    pub facts: Vec<CorpusFact>,
    pub queries: Vec<CorpusQuery>,
}

// ---------------------------------------------------------------------------
// Corpus epoch — all temporal offsets relative to this
// ---------------------------------------------------------------------------

fn corpus_epoch() -> DateTime<Utc> {
    // Fixed epoch: 2026-01-01T00:00:00Z
    DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("valid epoch")
        .with_timezone(&Utc)
}

// ---------------------------------------------------------------------------
// Golden corpus builder
// ---------------------------------------------------------------------------

/// Build the golden corpus.
///
/// Facts are indexed 0..N. Queries reference facts by index.
#[allow(clippy::too_many_lines)]
pub fn golden_corpus() -> CorpusDefinition {
    let epoch = corpus_epoch();

    let facts = vec![
        // =====================================================================
        // Domain 1: Software Development (indices 0-19)
        // =====================================================================

        // 0
        CorpusFact {
            content: "Rust ownership model prevents data races at compile time through the borrow checker",
            fact_type: FactType::Semantic,
            scope: Some("project:compiler"),
            opts: Some(FactOpts {
                importance: Some(0.9),
                ..default_opts()
            }),
        },
        // 1
        CorpusFact {
            content: "We deployed version 2.3.1 to production on March 15 2026",
            fact_type: FactType::Episodic,
            scope: Some("project:webapp"),
            opts: Some(FactOpts {
                importance: Some(0.6),
                t_created: Some(epoch + Duration::days(74)),
                ..default_opts()
            }),
        },
        // 2
        CorpusFact {
            content: "The CI pipeline uses GitHub Actions with three parallel jobs for build test and deploy",
            fact_type: FactType::Procedural,
            scope: Some("project:webapp"),
            opts: None,
        },
        // 3 (superseded by 4 — old protocol)
        CorpusFact {
            content: "Inter-service communication uses REST API with JSON payloads",
            fact_type: FactType::Semantic,
            scope: Some("project:webapp"),
            opts: Some(FactOpts {
                importance: Some(0.3),
                t_invalid: Some(epoch + Duration::days(60)),
                ..default_opts()
            }),
        },
        // 4 (supersedes 3 — new protocol)
        CorpusFact {
            content: "Switched from REST to gRPC for inter-service communication for lower latency",
            fact_type: FactType::Semantic,
            scope: Some("project:webapp"),
            opts: Some(FactOpts {
                importance: Some(0.8),
                t_valid: Some(epoch + Duration::days(60)),
                ..default_opts()
            }),
        },
        // 5
        CorpusFact {
            content: "SQLite WAL mode provides concurrent read access without blocking writers",
            fact_type: FactType::Semantic,
            scope: Some("project:compiler"),
            opts: Some(FactOpts {
                importance: Some(0.7),
                ..default_opts()
            }),
        },
        // 6
        CorpusFact {
            content: "The memory engine uses FTS5 full-text search index for keyword retrieval",
            fact_type: FactType::Semantic,
            scope: Some("project:compiler"),
            opts: None,
        },
        // 7
        CorpusFact {
            content: "Debug builds compile in 12 seconds but release builds take 4 minutes",
            fact_type: FactType::Episodic,
            scope: Some("project:compiler"),
            opts: Some(FactOpts {
                importance: Some(0.4),
                ..default_opts()
            }),
        },
        // 8
        CorpusFact {
            content: "To run integration tests use cargo test with the all-features flag",
            fact_type: FactType::Procedural,
            scope: Some("project:compiler"),
            opts: None,
        },
        // 9
        CorpusFact {
            content: "Clippy pedantic and nursery lints catch subtle bugs before code review",
            fact_type: FactType::Semantic,
            scope: Some("project:compiler"),
            opts: Some(FactOpts {
                importance: Some(0.5),
                ..default_opts()
            }),
        },
        // 10
        CorpusFact {
            content: "The graph module uses petgraph for in-memory adjacency list representation",
            fact_type: FactType::Semantic,
            scope: Some("project:compiler"),
            opts: None,
        },
        // 11
        CorpusFact {
            content: "Vector search uses cosine similarity with brute-force scan as default strategy",
            fact_type: FactType::Semantic,
            scope: Some("project:compiler"),
            opts: Some(FactOpts {
                importance: Some(0.7),
                ..default_opts()
            }),
        },
        // 12
        CorpusFact {
            content: "HNSW approximate nearest neighbor index accelerates vector search for large datasets",
            fact_type: FactType::Semantic,
            scope: Some("project:compiler"),
            opts: Some(FactOpts {
                importance: Some(0.6),
                ..default_opts()
            }),
        },
        // 13
        CorpusFact {
            content: "Connection pool uses N reader connections and one exclusive writer connection",
            fact_type: FactType::Semantic,
            scope: Some("project:compiler"),
            opts: None,
        },
        // 14
        CorpusFact {
            content: "Event sourcing stores all mutations as append-only events for audit trail",
            fact_type: FactType::Semantic,
            scope: Some("project:compiler"),
            opts: Some(FactOpts {
                importance: Some(0.8),
                ..default_opts()
            }),
        },
        // 15
        CorpusFact {
            content: "Schema migration uses VACUUM INTO for pre-migration backup safety",
            fact_type: FactType::Procedural,
            scope: Some("project:compiler"),
            opts: None,
        },
        // 16
        CorpusFact {
            content: "The reranker trait provides cross-encoder reranking for improved search quality",
            fact_type: FactType::Semantic,
            scope: Some("project:compiler"),
            opts: Some(FactOpts {
                importance: Some(0.6),
                ..default_opts()
            }),
        },
        // 17
        CorpusFact {
            content: "Ebbinghaus forgetting curve with multi-signal importance scoring for memory decay",
            fact_type: FactType::Semantic,
            scope: Some("project:compiler"),
            opts: Some(FactOpts {
                importance: Some(0.8),
                ..default_opts()
            }),
        },
        // 18
        CorpusFact {
            content: "The consolidation pipeline runs dedup then cluster then global summary in three passes",
            fact_type: FactType::Procedural,
            scope: Some("project:compiler"),
            opts: None,
        },
        // 19
        CorpusFact {
            content: "Bi-temporal model tracks four timestamps: created expired valid invalid per fact",
            fact_type: FactType::Semantic,
            scope: Some("project:compiler"),
            opts: Some(FactOpts {
                importance: Some(0.9),
                ..default_opts()
            }),
        },
        // =====================================================================
        // Domain 2: Research / ML (indices 20-34)
        // =====================================================================

        // 20
        CorpusFact {
            content: "Transformer attention mechanism has O(n^2) complexity in sequence length",
            fact_type: FactType::Semantic,
            scope: Some("research:ml"),
            opts: Some(FactOpts {
                importance: Some(0.8),
                ..default_opts()
            }),
        },
        // 21
        CorpusFact {
            content: "Ran experiment 42 with learning rate 3e-4 and achieved 0.92 F1 score on validation",
            fact_type: FactType::Episodic,
            scope: Some("research:ml"),
            opts: Some(FactOpts {
                importance: Some(0.5),
                t_created: Some(epoch + Duration::days(30)),
                ..default_opts()
            }),
        },
        // 22
        CorpusFact {
            content: "RAG retrieval augmented generation improves factual accuracy by grounding in documents",
            fact_type: FactType::Semantic,
            scope: Some("research:ml"),
            opts: None,
        },
        // 23
        CorpusFact {
            content: "Flash attention reduces memory usage from O(n^2) to O(n) for transformer training",
            fact_type: FactType::Semantic,
            scope: Some("research:ml"),
            opts: Some(FactOpts {
                importance: Some(0.7),
                ..default_opts()
            }),
        },
        // 24
        CorpusFact {
            content: "LoRA low-rank adaptation fine-tunes large language models with minimal parameters",
            fact_type: FactType::Semantic,
            scope: Some("research:ml"),
            opts: None,
        },
        // 25
        CorpusFact {
            content: "The embedding dimension of 768 matches BERT-base hidden size for compatibility",
            fact_type: FactType::Semantic,
            scope: Some("research:ml"),
            opts: Some(FactOpts {
                importance: Some(0.4),
                ..default_opts()
            }),
        },
        // 26
        CorpusFact {
            content: "Contrastive learning with InfoNCE loss trains effective text embedding models",
            fact_type: FactType::Semantic,
            scope: Some("research:ml"),
            opts: None,
        },
        // 27
        CorpusFact {
            content: "Benchmark results show MTEB leaderboard scores vary significantly across task types",
            fact_type: FactType::Episodic,
            scope: Some("research:ml"),
            opts: Some(FactOpts {
                importance: Some(0.5),
                ..default_opts()
            }),
        },
        // 28
        CorpusFact {
            content: "Knowledge distillation compresses large models into smaller student networks",
            fact_type: FactType::Semantic,
            scope: Some("research:ml"),
            opts: None,
        },
        // 29
        CorpusFact {
            content: "To train the model run python train.py with config yaml and checkpoint directory",
            fact_type: FactType::Procedural,
            scope: Some("research:ml"),
            opts: None,
        },
        // =====================================================================
        // Domain 3: Personal Context (indices 30-39)
        // =====================================================================

        // 30
        CorpusFact {
            content: "Prefers dark mode in all code editors and terminal applications",
            fact_type: FactType::Semantic,
            scope: Some("personal:preferences"),
            opts: Some(FactOpts {
                importance: Some(0.9),
                pinned: Some(true),
                ..default_opts()
            }),
        },
        // 31
        CorpusFact {
            content: "Had meeting with Alex about Q2 planning on March 10 2026",
            fact_type: FactType::Episodic,
            scope: Some("personal:calendar"),
            opts: Some(FactOpts {
                importance: Some(0.4),
                t_created: Some(epoch + Duration::days(69)),
                ..default_opts()
            }),
        },
        // 32
        CorpusFact {
            content: "Uses Neovim as primary editor with Lua-based configuration",
            fact_type: FactType::Semantic,
            scope: Some("personal:preferences"),
            opts: Some(FactOpts {
                importance: Some(0.7),
                pinned: Some(true),
                ..default_opts()
            }),
        },
        // 33
        CorpusFact {
            content: "PhD research focuses on mathematical morphology and image processing",
            fact_type: FactType::Semantic,
            scope: Some("personal:background"),
            opts: Some(FactOpts {
                importance: Some(0.9),
                pinned: Some(true),
                ..default_opts()
            }),
        },
        // 34
        CorpusFact {
            content: "Reminder to review pull request 187 before Friday deadline",
            fact_type: FactType::Episodic,
            scope: Some("personal:tasks"),
            opts: Some(FactOpts {
                importance: Some(0.6),
                t_valid: Some(epoch + Duration::days(90)),
                t_invalid: Some(epoch + Duration::days(95)),
                ..default_opts()
            }),
        },
        // 35
        CorpusFact {
            content: "Keyboard shortcut preference: Ctrl+P for command palette in all applications",
            fact_type: FactType::Procedural,
            scope: Some("personal:preferences"),
            opts: None,
        },
        // 36
        CorpusFact {
            content: "Daily standup is at 9:30 AM CET every weekday morning",
            fact_type: FactType::Procedural,
            scope: Some("personal:calendar"),
            opts: Some(FactOpts {
                importance: Some(0.5),
                ..default_opts()
            }),
        },
        // 37
        CorpusFact {
            content: "Completed the Rust async chapter study notes on February 20 2026",
            fact_type: FactType::Episodic,
            scope: Some("personal:learning"),
            opts: Some(FactOpts {
                importance: Some(0.3),
                t_created: Some(epoch + Duration::days(51)),
                last_accessed: Some(epoch + Duration::days(51)),
                ..default_opts()
            }),
        },
        // =====================================================================
        // Cross-cutting: varied importance + old/stale facts (indices 38-49)
        // =====================================================================

        // 38 — very low importance, should be forgotten easily
        CorpusFact {
            content: "Temporary note about fixing a typo in the readme file",
            fact_type: FactType::Episodic,
            scope: None,
            opts: Some(FactOpts {
                importance: Some(0.05),
                t_created: Some(epoch - Duration::days(120)),
                last_accessed: Some(epoch - Duration::days(120)),
                ..default_opts()
            }),
        },
        // 39 — high importance, frequently accessed
        CorpusFact {
            content: "The four-layer cognitive architecture: Knowledge Memory Wisdom Intelligence",
            fact_type: FactType::Semantic,
            scope: None,
            opts: Some(FactOpts {
                importance: Some(0.95),
                ..default_opts()
            }),
        },
        // 40 — already expired
        CorpusFact {
            content: "Old database password was changed on January 5 and is no longer valid",
            fact_type: FactType::Episodic,
            scope: Some("project:webapp"),
            opts: Some(FactOpts {
                importance: Some(0.1),
                t_invalid: Some(epoch + Duration::days(5)),
                ..default_opts()
            }),
        },
        // 41 — future-valid fact
        CorpusFact {
            content: "New API rate limits take effect starting April 15 2026",
            fact_type: FactType::Semantic,
            scope: Some("project:webapp"),
            opts: Some(FactOpts {
                importance: Some(0.7),
                t_valid: Some(epoch + Duration::days(105)),
                ..default_opts()
            }),
        },
        // 42 — multi-scope relevant
        CorpusFact {
            content: "Embedding models map text to dense vector representations for similarity search",
            fact_type: FactType::Semantic,
            scope: Some("research:ml"),
            opts: Some(FactOpts {
                importance: Some(0.8),
                ..default_opts()
            }),
        },
        // 43 — near-duplicate of 42 (for dedup testing)
        CorpusFact {
            content: "Text embedding models produce dense vector representations enabling similarity search",
            fact_type: FactType::Semantic,
            scope: Some("research:ml"),
            opts: Some(FactOpts {
                importance: Some(0.7),
                ..default_opts()
            }),
        },
        // 44 — near-duplicate of 0 (for dedup testing)
        CorpusFact {
            content: "Rust ownership and borrow checker prevents data races at compile time",
            fact_type: FactType::Semantic,
            scope: Some("project:compiler"),
            opts: Some(FactOpts {
                importance: Some(0.8),
                ..default_opts()
            }),
        },
        // 45
        CorpusFact {
            content: "Scope tree enables hierarchical organization of facts by project and domain",
            fact_type: FactType::Semantic,
            scope: None,
            opts: None,
        },
        // 46
        CorpusFact {
            content: "Pinned facts are immune to the forgetting process and never get expired",
            fact_type: FactType::Semantic,
            scope: None,
            opts: Some(FactOpts {
                importance: Some(0.7),
                ..default_opts()
            }),
        },
        // 47
        CorpusFact {
            content: "Reciprocal Rank Fusion combines FTS and vector search scores for hybrid retrieval",
            fact_type: FactType::Semantic,
            scope: Some("project:compiler"),
            opts: Some(FactOpts {
                importance: Some(0.8),
                ..default_opts()
            }),
        },
        // 48 — old stale fact, not accessed in a long time
        CorpusFact {
            content: "Initial prototype used Python with SQLAlchemy before rewriting in Rust",
            fact_type: FactType::Episodic,
            scope: Some("project:compiler"),
            opts: Some(FactOpts {
                importance: Some(0.2),
                t_created: Some(epoch - Duration::days(180)),
                last_accessed: Some(epoch - Duration::days(180)),
                ..default_opts()
            }),
        },
        // 49
        CorpusFact {
            content: "Import and export use JSON dump format with optional gzip or zstd compression",
            fact_type: FactType::Procedural,
            scope: Some("project:compiler"),
            opts: None,
        },
    ];

    // =====================================================================
    // 25 Queries with graded relevance judgments
    // =====================================================================

    let queries = vec![
        // Q0: Basic semantic retrieval — Rust ownership
        CorpusQuery {
            text: "Rust ownership borrow checker",
            relevance: &[(0, 3), (44, 3)],
            scope: None,
            assert_hybrid: false,
            description: "basic semantic: Rust ownership",
        },
        // Q1: Episodic retrieval — deployment history
        CorpusQuery {
            text: "deployed production version",
            relevance: &[(1, 3)],
            scope: None,
            assert_hybrid: false,
            description: "episodic: deployment history",
        },
        // Q2: Supersession — inter-service protocol
        CorpusQuery {
            text: "inter service communication REST gRPC",
            relevance: &[(4, 3), (3, 2)],
            scope: None,
            assert_hybrid: false,
            description: "supersession: gRPC should rank above REST",
        },
        // Q3: Scoped — compiler project only
        CorpusQuery {
            text: "search retrieval FTS5",
            relevance: &[(6, 3)],
            scope: Some("project:compiler"),
            assert_hybrid: false,
            description: "scoped: search-related facts in compiler project",
        },
        // Q4: ML domain — transformer attention
        CorpusQuery {
            text: "transformer attention mechanism complexity",
            relevance: &[(20, 3), (23, 2)],
            scope: None,
            assert_hybrid: false,
            description: "ML domain: transformer architecture",
        },
        // Q5: Experiment results
        CorpusQuery {
            text: "experiment learning rate F1 score validation",
            relevance: &[(21, 3), (27, 1)],
            scope: None,
            assert_hybrid: false,
            description: "episodic: experiment results",
        },
        // Q6: Personal preferences — editor
        CorpusQuery {
            text: "dark mode editors",
            relevance: &[(30, 3)],
            scope: None,
            assert_hybrid: false,
            description: "personal: editor preferences",
        },
        // Q7: Hybrid participation — embedding models
        CorpusQuery {
            text: "embedding dense vector similarity",
            relevance: &[(42, 3), (43, 2)],
            scope: None,
            assert_hybrid: true,
            description: "hybrid: embedding models (assert vector participation)",
        },
        // Q8: Forgetting and decay
        CorpusQuery {
            text: "Ebbinghaus forgetting curve decay importance",
            relevance: &[(17, 3), (46, 2)],
            scope: None,
            assert_hybrid: false,
            description: "forgetting behavior",
        },
        // Q9: Consolidation pipeline
        CorpusQuery {
            text: "consolidation dedup cluster summary pipeline",
            relevance: &[(18, 3)],
            scope: None,
            assert_hybrid: false,
            description: "consolidation process",
        },
        // Q10: Bi-temporal model
        CorpusQuery {
            text: "temporal timestamps created expired valid invalid",
            relevance: &[(19, 3), (14, 1)],
            scope: None,
            assert_hybrid: false,
            description: "bi-temporal semantics",
        },
        // Q11: CI pipeline
        CorpusQuery {
            text: "CI pipeline GitHub Actions parallel jobs build test",
            relevance: &[(2, 3)],
            scope: None,
            assert_hybrid: false,
            description: "procedural: CI pipeline",
        },
        // Q12: Database and SQLite
        CorpusQuery {
            text: "SQLite WAL concurrent read",
            relevance: &[(5, 3), (13, 2)],
            scope: None,
            assert_hybrid: false,
            description: "database: SQLite internals",
        },
        // Q13: RAG and retrieval augmented generation
        CorpusQuery {
            text: "RAG retrieval augmented generation grounding documents",
            relevance: &[(22, 3)],
            scope: None,
            assert_hybrid: false,
            description: "ML: RAG pattern",
        },
        // Q14: Fine-tuning LLMs
        CorpusQuery {
            text: "LoRA adaptation language models parameters",
            relevance: &[(24, 3)],
            scope: None,
            assert_hybrid: false,
            description: "ML: fine-tuning methods",
        },
        // Q15: Hybrid search + RRF
        CorpusQuery {
            text: "hybrid search Reciprocal Rank Fusion FTS vector",
            relevance: &[(47, 3), (6, 2), (11, 2)],
            scope: None,
            assert_hybrid: true,
            description: "hybrid: search fusion (assert vector participation)",
        },
        // Q16: Cognitive architecture
        CorpusQuery {
            text: "four layer cognitive architecture Knowledge Memory Wisdom Intelligence",
            relevance: &[(39, 3)],
            scope: None,
            assert_hybrid: false,
            description: "architecture: four layers",
        },
        // Q17: Event sourcing and audit
        CorpusQuery {
            text: "event sourcing append only mutations audit trail",
            relevance: &[(14, 3)],
            scope: None,
            assert_hybrid: false,
            description: "event sourcing pattern",
        },
        // Q18: Research background — morphology
        CorpusQuery {
            text: "PhD research mathematical morphology image processing",
            relevance: &[(33, 3)],
            scope: None,
            assert_hybrid: false,
            description: "personal: research background",
        },
        // Q19: Scope tree and hierarchy
        CorpusQuery {
            text: "scope tree hierarchical organization project domain",
            relevance: &[(45, 3)],
            scope: None,
            assert_hybrid: false,
            description: "scope tree structure",
        },
        // Q20: Pinned facts immunity
        CorpusQuery {
            text: "pinned facts immune forgetting expired never",
            relevance: &[(46, 3), (17, 1)],
            scope: None,
            assert_hybrid: false,
            description: "pin immunity mechanism",
        },
        // Q21: Personal scope — calendar meetings
        CorpusQuery {
            text: "meeting planning",
            relevance: &[(31, 2)],
            scope: Some("personal:calendar"),
            assert_hybrid: false,
            description: "scoped: personal calendar",
        },
        // Q22: Schema migration
        CorpusQuery {
            text: "schema migration backup VACUUM INTO safety",
            relevance: &[(15, 3)],
            scope: None,
            assert_hybrid: false,
            description: "procedural: schema migration",
        },
        // Q23: Graph module
        CorpusQuery {
            text: "graph petgraph adjacency list memory",
            relevance: &[(10, 3)],
            scope: None,
            assert_hybrid: false,
            description: "graph representation",
        },
        // Q24: Hybrid participation — reranker cross-encoder
        CorpusQuery {
            text: "reranker cross encoder search quality improved",
            relevance: &[(16, 3), (47, 1)],
            scope: None,
            assert_hybrid: true,
            description: "hybrid: reranker (assert vector participation)",
        },
    ];

    CorpusDefinition { facts, queries }
}

fn default_opts() -> FactOpts {
    FactOpts {
        importance: None,
        pinned: None,
        t_valid: None,
        t_invalid: None,
        t_created: None,
        last_accessed: None,
    }
}
