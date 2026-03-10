# Round 3: Claude (Host) — Final Position

## 1. Memory: CONVERGE on event-sourced log + single multi-model DB

Codex's event-sourced log idea is the strongest primitive. Every agent action, observation, and decision goes into an append-only log. Materialized views project this into:

- Hot path: FTS5+vector search (SurrealDB or embedded)
- Warm path: Graph relationships (SurrealDB graph queries)
- Cold path: Markdown files on disk (human-readable, git-versioned)

**SurrealDB 3.0 vs FalkorDB**: SurrealDB has the better Rust story (Rust-native, multi-model). FalkorDB is Redis-based, C. I maintain SurrealDB is the right choice. But Codex is right that the graph should be DERIVED from the log, never the source of truth. This eliminates the synchronization problem Gemini identified.

## 2. Build: CONVERGE on modular monolith

All three advisors reject forking. Codex's "modular monolith" is the right framing — not "microservices" or "hexagonal ports" but a single Cargo workspace with strict module boundaries. Start with 4-5 crates:

- `core`: Agent loop, task scheduler, state machine
- `memory`: SurrealDB wrapper, event log, search
- `inference`: LLM abstraction (Rig-based, MLX+Ollama providers)
- `tools`: MCP client (rmcp), built-in tools
- `channels`: Telegram adapter (one channel only, YAGNI)

## 3. MLX: CONVERGE with nuance

Gemini's point about Candle/Ratchet for native Rust inference is interesting but premature — MLX has years of Apple Silicon optimization, Candle doesn't match it on M4 yet. mlx-lm's OpenAI-compatible API is good enough for now.

**Final position**: MLX for backbone via mlx-lm (or LM Studio MLX backend). Ollama on Jetson. Single Rig provider interface. No custom proxy.

## 4. Fine-tuning: PARTIAL CONVERGENCE

We all agree: don't fine-tune for factual knowledge. But Gemini raised a valid point in R2 — the base model won't learn the user's codebase "vibe" without some adaptation.

**Final position**:

- Primary memory: Retrieval-based (event log + SurrealDB)
- Style adaptation: Small periodic LoRA on codebase style/formatting (monthly, on 4090, not Jetson)
- Doc-to-LoRA: Track as Tier 3 research. If Qwen-compatible hypernetwork appears, re-evaluate.
- Retrieval ranker: Train bi-encoder on Jetson when enough labeled data (6+ months in).

## 5. Escalation: CONVERGE on dry-run + intent-locking

Gemini's intent-locking + my speculative dry-run + Codex's risk-scored matrix combine into:

- Agent plans actions, presents intent summary to user for high-risk ops
- Low-risk ops: autonomous, no confirmation needed
- Medium-risk: dry-run → diff preview → user approves via Telegram
- High-risk: blocked until human confirms
- Risk scored by: reversibility × impact × confidence

**Not hallucinate-and-rollback** (Gemini R1). We all agree this is dangerous.

---

## CONSENSUS POINTS (all 3 advisors agree)

1. **Memory**: Retrieval-based, not parametric. Event-sourced log as ground truth. Graph as derived view.
2. **Build**: New project, not fork. Modular monolith in Rust Cargo workspace.
3. **Inference**: Dual-runtime (MLX + Ollama) with unified provider interface.
4. **Fine-tuning**: Don't fine-tune for knowledge. Retrieval handles factual memory.
5. **Escalation**: Async, non-blocking. Agent continues other tasks while waiting. Risk-scored.
6. **Security**: MCP over stdio, sandboxed, no public exposure.

## OPEN DISAGREEMENTS

1. **SurrealDB vs FalkorDB vs event-log-only**: Which multi-model DB? Or skip DB and use raw event log + SQLite + Petgraph?
2. **mlx-lm vs Candle/Ratchet**: Research-grade Python server vs native Rust (immature)? Wait for Rust MLX bindings?
3. **Style LoRA**: Is periodic style fine-tuning worth the engineering cost? Or does in-context learning suffice?
4. **Jetson role**: Essential satellite or unnecessary complexity? All agree it's "optional" but disagree on when it becomes worth it.
5. **Inference gateway**: Single canonical gateway (Codex) vs dual-runtime-by-default (Claude/Gemini)?
