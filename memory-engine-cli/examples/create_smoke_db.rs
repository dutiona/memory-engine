use memory_engine::{EmbeddingProvider, EngineConfig, FactType, MemoryEngine};

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
            "User prefers concise responses without filler words",
            FactType::Semantic,
            None,
            &e,
            None,
            None,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            "Project uses Rust 2024 edition with clap for CLI parsing",
            FactType::Procedural,
            None,
            &e,
            None,
            None,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            "Meeting discussed Q2 roadmap: Phase 4b CLI inspector then MCP server",
            FactType::Episodic,
            None,
            &e,
            None,
            None,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            "The database schema migrated from v5 to v6 on 2026-03-15",
            FactType::Semantic,
            None,
            &e,
            None,
            None,
            None,
        )
        .unwrap();
    engine
        .add_fact(
            "Agent successfully resolved 3 merge conflicts in the auth module",
            FactType::Episodic,
            None,
            &e,
            None,
            None,
            None,
        )
        .unwrap();
    engine.pin_fact(1).unwrap();

    println!("Created /tmp/smoke-test-agent.db with 5 facts (fact 1 pinned)");
}
