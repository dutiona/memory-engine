use memory_engine::{AddFactRequest, EmbeddingProvider, EngineConfig, FactType, MemoryEngine};

struct FakeEmbed;
impl EmbeddingProvider for FakeEmbed {
    fn embed(&self, _text: &str) -> memory_engine::Result<Vec<f32>> {
        Ok(vec![0.1, 0.2, 0.3, 0.4])
    }
}

fn main() {
    let path = std::path::PathBuf::from("/tmp/smoke-test-agent.db");
    if path.exists() {
        std::fs::remove_file(&path).unwrap();
    }
    let config = EngineConfig::new(path, 4);
    let engine = MemoryEngine::open(&config).unwrap();
    let e = FakeEmbed;

    engine
        .add_fact(
            &AddFactRequest {
                content: "User prefers concise responses without filler words".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &e,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "Project uses Rust 2024 edition with clap for CLI parsing".into(),
                fact_type: FactType::Procedural,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &e,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "Meeting discussed Q2 roadmap: Phase 4b CLI inspector then MCP server"
                    .into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &e,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "The database schema migrated from v5 to v6 on 2026-03-15".into(),
                fact_type: FactType::Semantic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &e,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            &AddFactRequest {
                content: "Agent successfully resolved 3 merge conflicts in the auth module".into(),
                fact_type: FactType::Episodic,
                source_event_id: None,
                scope: None,
                opts: None,
            },
            &e,
            None,
        )
        .unwrap();
    engine.pin_fact(1).unwrap();

    println!("Created /tmp/smoke-test-agent.db with 5 facts (fact 1 pinned)");
}
