### Round 3: Final Architectural Position for Project Panther

**1. Memory: The Event-Sourced Unified Store**
We converge on a **Unified Multi-Model Store** (e.g., SurrealDB or FalkorDB) to eliminate the "temporal lobotomy" of split vector/graph planes. By treating vector embeddings as first-class properties of graph nodes, we ensure transactional integrity between semantic and structural data. However, we adopt Codex’s mandate for an **Event-Sourced Ledger** as the ultimate source of truth. Raw interactions and filesystem changes are stored in an append-only log; the unified DB acts as a materialized view. This allows for risk-free re-indexing when embedding models or ontologies evolve. We move from "Vector vs. Graph" to a "Structured Contextual Stream" where the graph is the map, the vector is the compass, and the event-log is the history.

**2. Build: Rust Core with FFI Flexibility**
The consensus is a **Native Rust Core** following a modular monolith pattern. Rust’s concurrency and type-safety are non-negotiable for a reliable multi-agent orchestrator. We reject a pure Python build as "extraction" from dynamic debt is a common fallacy; however, we adopt Gemini’s **PyO3/FFI bridge** for "Hot Logic." This allows Panther to leverage the Python ML ecosystem for rapid experimentation while keeping "Cold Logic" (persistence, scheduling, state machines) in a high-performance Rust binary. Following Claude’s YAGNI principle, we prioritize CLI and a single messaging bridge (Telegram) over complex microservices, ensuring the core remains "slimmer" and more portable across dev environments.

**3. MLX & Inference: Native-First Gateway**
Panther will utilize a **Unified Inference Gateway** that abstracts backends but defaults to **Native Rust Inference (Candle or Ratchet)** for the inner loop. While `mlx-lm` provides immediate Apple Silicon parity, a native Rust engine eliminates Python runtime overhead, targeting sub-10ms TTFT for agentic "thinking." We adopt Claude’s "No Proxy" stance: if local engines provide OpenAI-compatible endpoints, we consume them directly to minimize latency. The gateway’s primary role is not translation, but **Quantization Management**—dynamically switching between 4-bit and 8-bit models based on the task’s complexity and the machine’s thermal/memory pressure, ensuring Panther feels "alive" and snappy.

**4. Fine-Tuning: Behavior Adaptation, Not Knowledge**
We establish a **PEFT-Lite** strategy where LoRA adaptation is reserved strictly for **Behavior and Style**, never for fact retrieval. Knowledge belongs in the Unified Store. We adopt Gemini’s "Continuous PEFT" for language/vibe—allowing Panther to learn the user’s specific naming conventions and "voice." However, we gate this with Codex’s **Counterfactual Evaluation**: a LoRA is only activated if it outperforms the base model + RAG on user-history benchmarks. This prevents "catastrophic forgetting" and ensures that fine-tuning is an evolution, not a liability. Panther will "learn" how the user writes code, but will "lookup" how the user’s code actually works.

**5. Escalation & Control: Intent-Locked Speculation**
Safety will be managed via **Intent-Locked Speculative Execution**. We move away from reactive rollbacks toward Gemini’s "Intent Diff." Before any filesystem modification, Panther must generate a high-level summary of its *intent*, which the user approves before the model generates the specific code or command. This is supported by a **Risk-Scored Escalation Matrix** (Codex) where operations exceeding a risk threshold (e.g., `git push`, `rm -rf`, `brew install`) require mandatory human-in-the-loop confirmation. We reject Claude’s rollback-heavy approach as insufficient for destructive CLI actions; control must be moved upstream to the "Decision Node" rather than the "Output Node."

***

### CONSENSUS POINTS
*   **Unified Persistence:** Single multi-model DB (Graph + Vector + KV) is superior to polyglot sync-hell.
*   **Rust Core:** Performance and safety demand a Rust-based orchestrator, not pure Python.
*   **Local-First:** Primary inference must be local (Apple Silicon/NVIDIA) to support agentic loops.
*   **Intent-Based Safety:** User approval must happen at the "Intent" level, not just the "Code" level.
*   **RAG over PEFT for Facts:** Fine-tuning should never be used as a primary knowledge storage mechanism.

### OPEN DISAGREEMENTS
*   **Event-Sourcing Necessity:** Codex views the Event Log as the primary source of truth; Claude and Gemini see it as a "nice-to-have" audit trail.
*   **ML Runtime:** Disagreement remains on whether to use `mlx-lm` (Python) for parity or Candle (Rust) for speed-of-execution.
*   **LoRA Frequency:** Whether PEFT should be a continuous background process (Gemini) or a discrete "Tier 3" research event (Claude).
