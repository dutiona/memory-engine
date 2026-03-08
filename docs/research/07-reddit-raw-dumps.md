# Raw Research Dumps — Reddit & Community Sources

**Purpose:** Incremental capture of research findings. Written immediately on receipt to survive context compaction.

---

## Dump 1: r/openclaw 3-Layer Memory (agent ae1196b2dbf36325c, completed 2026-03-08)

**Post:** u/duridsukar on r/openclaw — "I built a 3-layer memory system that stopped my agents from starting every session from zero"
**URL:** https://www.reddit.com/r/openclaw/comments/1rnku5b/

### Architecture

| Layer         | Files                                                                       | Access                              | Budget                          |
| ------------- | --------------------------------------------------------------------------- | ----------------------------------- | ------------------------------- |
| L1: Brain     | SOUL.md, AGENTS.md, MEMORY.md, USER.md, TOOLS.md, IDENTITY.md, HEARTBEAT.md | Always loaded every turn            | 500-1000 tokens/file, ~7K total |
| L2: Memory    | `memory/YYYY-MM-DD.md` (daily notes) + `memory/[topic].md` (breadcrumbs)    | Semantic search via `memory_search` | 4KB/file max                    |
| L3: Reference | SOPs, playbooks, research in `reference/`                                   | On-demand explicit read             | Unlimited                       |

### Five Trigger Protocols

| Trigger       | Purpose                                                                                                           |
| ------------- | ----------------------------------------------------------------------------------------------------------------- |
| `recover`     | Full context rebuild after reset/compaction                                                                       |
| `checkpoint`  | Capture session state, route by type to files. **Auto-fires before compaction** (~25K tokens before window fills) |
| `trim`        | Maintenance: measure L1 files, archive excess to L2/L3                                                            |
| `recalibrate` | Drift correction: re-read all L1, compare recent behavior, report deviations                                      |
| `checkboard`  | Project board dump: all projects by status                                                                        |

### Key Insights

- "More words = less focus" — agents pattern-match/skim bloated files, don't know they skimmed
- Brevity is an architecture decision, not a style preference
- Auto-compaction checkpoint injection is the critical safety net
- Breadcrumbs bridge fast recall (L2) and deep reference (L3)
- No code repo shared — system is file-organization convention + prompt engineering

### Community Feedback

- Auto-compaction checkpoint praised as "genius" (u/Available_Cupcake298)
- QMD mentioned as complementary ("night and day difference") — not elaborated
- No benchmarks, no repo, some skepticism about sales-pitch tone

### Related r/openclaw Memory Posts (high signal)

- "We built persistent memory for OpenClaw" (59↑, 34 comments)
- "380us hybrid vector+BM25 search, single .h5 file" (68↑, 52 comments)
- "I left two AI agents alone in a Discord channel overnight. They built their own memory system." (296↑, 94 comments)
- github.com/coolmanns/openclaw-memory-architecture (SQLite + FTS5 + Semantic Search)
- "PSA: Turn on memory search with embeddings" (97↑, 39 comments)

---

## Dump 2: OpenClaw Ecosystem (agent abac94b8fcc903cfd, completed 2026-03-08)

**Sources:** Wikipedia, GitHub, DeepWiki, OpenClaw Docs, Medium articles, DEV Community, Agentailor blog, Nebius security blog

### Core Facts

- 247k GitHub stars, 47.7k forks (Mar 2026). 210k stars in first 10 days.
- Created by Peter Steinberger (Austrian). Renamed from Clawdbot → Moltbot → OpenClaw.
- Steinberger joining OpenAI, project moving to open-source foundation (Feb 2026).
- Security: CVE-2026-25253 (one-click RCE, CVSS 8.8), CVE-2026-26327 (auth bypass). "ClawHavoc" supply chain attack.

### Architecture

- **Gateway** — long-lived WebSocket/HTTP process (port 18789), central control plane
- **Agent Runtime** — AI loop: assemble context → invoke model → execute tools → persist state
- **Channels** — provider pattern: WhatsApp, Telegram, Discord, Slack, Signal, iMessage, web UI, CLI
- **Lane Queue** — serialized per-session execution (no concurrent tool calls per session)
- **Tool policy** — hierarchical: global → agent → group → sandbox
- **Workspace-first** — SOUL.md, TOOLS.md, IDENTITY.md, HEARTBEAT.md define agent behavior

### Memory (4-layer)

1. Session context (in-memory)
2. Daily logs (`memory/YYYY-MM-DD/`, markdown)
3. Long-term (`MEMORY.md`, curated insights, markdown)
4. Semantic search (SQLite + sqlite-vec, FTS5 + BM25 + cosine)

Markdown is canonical, SQLite is derived. Pre-compaction memory flush via silent turn.

### ZeroClaw (Rust rewrite)

- 3.4MB binary, <5MB RAM, <10ms startup
- Trait-driven modular architecture
- 22+ AI providers, 70+ integrations
- SQLite primary, PostgreSQL + Markdown alternatives
- Built-in vector search (cosine + FTS5 + hybrid merging)
- Config-compatible with OpenClaw
- 24.5k stars

### Other Forks

| Fork     | Language | Focus                                             |
| -------- | -------- | ------------------------------------------------- |
| PicoClaw | Go       | IoT/embedded, $10 RISC-V boards                   |
| NanoClaw | —        | Security-hardened, container isolation, audit log |
| IronClaw | —        | Mentioned, no details                             |
| MaxClaw  | —        | By MiniMax                                        |
| KimiClaw | —        | By Moonshot AI                                    |

### Patterns to Adopt

1. Gateway + Agent Runtime separation
2. Markdown-canonical memory with SQLite index
3. Lane Queue (serialized per-session)
4. Workspace-first config (version-controllable)
5. Trait-based modularity (ZeroClaw)
6. Pre-compaction memory flush
7. Hierarchical tool policy
8. Three autonomy levels (readonly, supervised, full)

---

## Dump 3: Second Brain & Self-Improving Claude Code (agent aa9c38fd621555e54, completed 2026-03-08)

**Post 1:** r/ClaudeCode — "Anyone actually built a second brain that isn't just a graveyard of saved links?"
**Post 2:** r/ClaudeCode — "How We Turned Claude Code Into a Self-Improving AI Engineering Platform"

### Mengram (u/No_Advertising2536)

- **3-path retrieval:** semantic (facts), episodic (events with outcomes), procedural (self-versioning workflows)
- Knowledge graph on top of embeddings
- 3 Claude Code hooks: profile on session start, memory search on every prompt, save after responses
- Obsidian import command
- **Procedural memory versions on failure** — workflows evolve without human intervention
- Repo: github.com/alibaizhanov/mengram
- Obsidian plugin: github.com/alibaizhanov/obsidian-mengram

### Ori Mnemos (u/Beneficial_Carry_530)

- Markdown files with YAML metadata + wiki-links → traversable graph
- 384-dim embeddings (MiniLM) for semantic search
- MCP server interface
- **Graph-aware forgetting** — prunes low-value notes based on connectivity (isolated nodes pruned)
- Repo: github.com/aayoawoyemi/Ori-Mnemos
- Site: orimnemos.com

### Wolly_Bolly's Pipeline

- QuickCapture (iPhone) → cron agent → structured Obsidian vault → 4 Kanban boards as triage
- Global `/cerebro` skill for vault interaction
- Hot capture / cold organization separation
- "Triage part still has rough edges"

### `/insights` Feedback Loop (Vishal Sachdev, Substack)

- Work → `/insights` analyzes sessions → report → fixes added to CLAUDE.md → repeat
- Concrete friction patterns found: Claude searching filesystem for general knowledge (9×), observer agents wasteful for short sessions (20% overhead saved), pre-deployment constraint checks needed
- Meta-loop: Feed entire insights report to Claude to rewrite CLAUDE.md
- "CLAUDE.md is the onboarding doc; `/insights` is the performance review. Each cycle raises the floor."
- Gist: vishalsachdev/2f2a0e339616548bc42a131b95a0eb85
- Article: chatwithgpt.substack.com/p/the-self-improving-loop-how-claude

### Community Skepticism

- kubrador: "The capture layer is what matters and everything else is cope" — semantic search doesn't replace tagging
- TailorImaginary3629: "Unless you invest your own time to research, it's still a grave"
- Amazing-Cup-2601: "A lot of generic BS imo" (re: enterprise scaling post)
- Bosun (github.com/virtengine/bosun) cited as "actually self-improving"

### Synthesis: Novel Ideas Not in Academic Papers

1. **Graph-aware forgetting** (Ori Mnemos) — graph topology as proxy for memory importance
2. **Procedure versioning on failure** (Mengram) — living procedural memory that evolves
3. **`/insights` meta-loop** — agent reviews own telemetry, modifies own system prompt (instruction-level self-modification)
4. **kubrador's anti-pattern** — write-time quality gate > downstream retrieval sophistication (validates ACE curation paper)
5. **Hot capture / cold organization** — separate fast capture path from cron-based organization path

---

## Dump 4: Claude Code Self-Improvement v2 (agent a77887dc7af70ff6d, 2026-03-08)

**Status:** Agent could not access the provided Reddit URLs (returned 404/redirects). The URLs `1lh5z8r` and `1lgxq2p` from the conversation summary may have been hallucinated during context compaction or may reference posts not accessible to scrapers.

**Gap:** No new data from this agent. All substantive findings on this topic came from Dump 3 (agent aa9c38fd621555e54).

---

## Cross-Reference Notes

- Dumps 1-3 are consistent and mutually reinforcing
- OpenClaw's 4-layer (Dump 2) maps cleanly to Reddit's 3-layer (Dump 1) — Reddit author added L3:Reference on top
- Mengram's 3-type memory (Dump 3) is orthogonal to the layer hierarchy — it classifies by content type, not access pattern
- All sources independently validate CoALA's working/episodic/semantic/procedural taxonomy
- The strongest novel finding across all dumps: **write-time quality gating** is more important than retrieval sophistication (kubrador + ACE curation + OpenClaw's pre-compaction flush all point the same way)
