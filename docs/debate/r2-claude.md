# Round 2: Claude (Host) — Challenges

## 1. Memory: All three of us may be overcomplicating this

**Challenge to all positions**: We're all proposing multi-DB architectures (Memori + graph, tri-store, etc.) when our memory research agent just found **SurrealDB 3.0** — a single Rust-based DB that combines vector, graph, KV, and time-series. It eliminates the composition problem entirely. Single process, single schema, Rust SDK.

**Challenge to Gemini's SurrealDB/EdgeDB suggestion**: SurrealDB 3.0 exists NOW. Why propose it alongside Memori? Replace Memori entirely with SurrealDB's hybrid queries. One DB, one connection, one schema migration.

**Challenge to Codex's "async graph promotion"**: If the graph and search live in the same DB (SurrealDB), promotion is a SQL update, not an async pipeline. The confidence threshold + validator idea is right, but the infrastructure is simpler than proposed.

**My updated position**: SurrealDB 3.0 as the single memory store. Markdown files for core/procedural memory (human-readable layer). No separate vector DB, no separate graph DB.

## 2. Build: We all agree — but is Panther really the right reference?

**Challenge to all**: We unanimously chose "new project, extract patterns." But Panther has 9 crates and sophisticated multi-channel support. Do we NEED multi-channel from day 1? YAGNI says start with CLI + one messaging channel (Telegram). The Panther architecture may be over-indexed on.

**Stronger reference**: ZeroClaw's trait system is more relevant than Panther's daemon model. Start with ZeroClaw-style traits + Rig for LLM + rmcp for MCP. Add channels later.

## 3. MLX: We agree. But there's a hidden dependency problem.

**Challenge to Gemini's "Rust proxy"**: mlx-lm already exposes an OpenAI-compatible API. Why build a Rust proxy? The proxy adds a failure point and maintenance burden. Just point Rig's OpenAI provider at mlx-lm's endpoint.

**Challenge to Codex's "conformance tests"**: Good idea in theory, but the behavioral drift between MLX and Ollama is MODEL-level, not engine-level. Same model weights, same tokenizer. The real risk is quantization differences (MLX quantizes differently than GGUF). Test at the quant level, not the engine level.

## 4. Memory model: Are we dismissing fine-tuning too quickly?

**Challenge to all (including myself)**: We unanimously rejected parametric memory. But **Doc-to-LoRA** (Sakana AI, Feb 2026) generates LoRA adapters from documents in a single forward pass — no training loop, no catastrophic forgetting, each document gets its own adapter. If a Qwen-compatible hypernetwork exists or can be trained, this changes the calculus entirely.

**Also**: Sparse memory fine-tuning achieves only 11% knowledge drop (vs 89% for full FT). This is NOT catastrophic. We should keep this as a Tier 3 research item, not dismiss it.

## 5. Escalation: Gemini's "hallucinate predicted response" is dangerous

**Direct challenge to Gemini**: Having the agent "hallucinate a predicted Oracle response" and then rollback if wrong is playing with fire. If the agent acts on a hallucinated Opus response and makes irreversible changes (git push, file deletion, API call), rollback may be impossible. State rollback is only safe for pure-computation tasks.

**Stronger position**: Don't predict. Instead, use **speculative execution with dry-run mode**: the agent plans what it WOULD do with an Oracle response, but doesn't execute. When the real response arrives, it validates against the plan and executes. This preserves the non-blocking property without the rollback risk.
