# Contributing to memory-engine

## Getting Started

```bash
git clone https://github.com/dutiona/memory-engine.git
cd memory-engine
cargo build
cargo test
```

**Requirements:** Rust 1.88+ (edition 2024). No external services needed — SQLite is bundled.

## Development

### Build & Test

```bash
cargo build                    # debug build
cargo test                     # run all tests
cargo test --doc               # doc examples only
cargo clippy --all-targets     # lint
cargo fmt --check              # format check
```

### Run Examples

```bash
cargo run --example basic_roundtrip
cargo run --example bi_temporal_query
cargo run --example custom_traits
```

### Documentation

```bash
cargo doc --no-deps --open     # API reference
uv run sphinx-build -b html docs docs/_build  # narrative docs (requires Python 3.12+)
```

## Code Style

- `cargo fmt` with default settings
- `cargo clippy` — all warnings addressed, pedantic enabled
- `unsafe_code = "forbid"` — no unsafe code
- Error handling: `thiserror` for all error types, `Result<T, MemoryError>` everywhere
- Document all public items with rustdoc (including `# Errors` and `# Examples` sections)

## Architecture

The engine uses trait-based extensibility. Core crate has zero network or LLM dependencies:

- `EmbeddingProvider` — consumers bring their own embedding model
- `SummaryGenerator` — consumers bring their own summarization
- `ConflictArbiter` — consumers define conflict resolution logic

See [docs/design/architecture-overview.md](docs/design/architecture-overview.md) for the full picture.

## Pull Requests

1. Branch from `main`
2. One logical change per PR
3. All tests must pass (`cargo test`)
4. No clippy warnings (`cargo clippy --all-targets`)
5. Formatted (`cargo fmt --check`)
6. New public API items must have rustdoc with examples

## License

By contributing, you agree that your contributions will be licensed under `MIT OR Apache-2.0`.
