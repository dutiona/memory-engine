# feat(embed): pin the served embedding identity and refuse to start on a mismatch (#1017)

## Outcome

ME's embedding identity gains the **served artifact**: the served model file's SHA-256, the pooling, the
normalization and the maximum input length in tokens. It is one optional sub-struct of `EmbeddingFingerprint`, so
every existing identity check (first-write stamp, startup check, reconstruction resume, dump/restore) compares it
with no new call sites. SQLite schema v15 stores it with the embedding space. The MCP server declares it through TOML,
flags or environment, and exits non-zero before serving when any one identity field differs from the store's; a
process-level test proves this per field. `HttpEmbeddingProvider` prefixes ME's pinned instruction to every query by
default and never to a document. `llama.cpp` is a known provider. Postgres refuses a served fingerprint until #634
mirrors the columns. ADR-0015 pins the tuple and both stores' query instructions in text dutiona/knowledge-base#573
can copy byte-identical.

> **APPROVAL POINT for Michaël: the Knowledge-layer query instruction.** ADR-0015 §6 records, verbatim (`\n` is one
> line feed; the string ends with one space after `Query:`):
>
> ```text
> Instruct: Given a search query, retrieve relevant document passages.\nQuery: 
> ```
>
> ME's code never contains this string; only the ADR row does. Approving this plan approves the string.

## Constraints

- ADR-0015 records the tuple: `Qwen/Qwen3-Embedding-0.6B` (GGUF Q8_0), provider `llama.cpp`, dim 1024.
- The served artifact: the sha256 of the official `Qwen/Qwen3-Embedding-0.6B-GGUF` Q8_0 file (the file the 2026-11-02 benchmark used), last-token pooling, L2 normalization, and a bounded maximum input length in tokens (proposed 2,048, well above the 538-token benchmark chunks; dutiona/infra#77 confirms it fits in the Mac's memory with the chosen `--parallel`).
- The file: Hugging Face `Qwen/Qwen3-Embedding-0.6B-GGUF` at commit `370f27d7550e0def9b39c1f16d3fbaa13aa67728`, `Qwen3-Embedding-0.6B-Q8_0.gguf`, 639,150,592 bytes, LFS sha256 `06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439` (fetched 2026-11-16 from the HF API; the benchmark machine's copy was deleted, so dutiona/infra#77 verifies the installed file against this value).
- ME keeps `Instruct: Given a search query, retrieve relevant memory facts.\nQuery: `; KB gets `Instruct: Given a search query, retrieve relevant document passages.\nQuery: `, proposed here and awaiting Michaël's confirmation (the APPROVAL POINT).
- The shared text is written as it will be true after dutiona/knowledge-base#573: both stores detect a mismatch, and reconstruction is described for both.
- The served-artifact fields are stored with the embedding space and compared field by field at startup. One PR; existing fingerprint constructors keep working.
- Process-level test: started against a store stamped with a different value in any one field, the server exits non-zero before serving.
- The client adds the pinned instruction to queries by default and never to documents; a wiremock test asserts the request bodies.
- `llama.cpp` is a known provider. New SQLite columns are listed on #634 for Postgres parity, and the schema-parity proof runs.
- Boy-scout, in its own commit: `embedding_args.rs` stops naming the non-existent `--embed-dimensions`.
- "Each embedding client adds its store's instruction to queries automatically; callers never build it." (spec 7)
- Accepted residual: the sha256 is declared and compared, never hashed or probed at runtime.
- #1021 owns truncating inputs to `max_input_tokens`; here it is identity only.
- #1018 edits `crates/memory-engine-mcp/src/{main,config}.rs` and removes stdio; the second to land rebases. Both
  stdio flows of the process test (serving and refusing, item 9) are transport-specific; #1018 rewrites both.
- Do not touch `crates/me-backend-sqlite/src/store/schema/pragmas.rs` (#1040).
- Public repository: no tailnet name, address or token anywhere.

## Decisions

1. **Type.** `me-types` gains `ServedArtifact { artifact_sha256: String, pooling: String, normalization: String,
   max_input_tokens: usize }` (derives as `EmbeddingFingerprint`, plus `#[serde(deny_unknown_fields)]`), and
   `EmbeddingFingerprint` gains `#[serde(default, skip_serializing_if = "Option::is_none")] pub served:
   Option<ServedArtifact>`. `new`/`with_matryoshka` keep their signatures and set `None`; `with_served(self,
   ServedArtifact) -> Self` attaches one. A sub-struct makes a half-declared artifact unrepresentable; legacy JSON and
   unserved dumps stay byte-identical. `artifact_sha256`, not `gguf_sha256`: the names are normative in two
   repositories, and the ADR defines it as the SHA-256 of the single model file the provider serves (the GGUF for
   llama.cpp).
2. **One validating boundary.** `ServedArtifact::from_parts(sha, pooling, normalization, max_tokens: Option<…>) ->
   me_types::error::Result<Option<Self>>`: all `None` → `Ok(None)`; all `Some` → validated `Some`; otherwise
   `MemoryError::Conflict(ConflictError::PolicyParameter(..))` naming the missing or invalid fields (AGENTS.md rule
   9: typed errors, `Result<T, MemoryError>`). Validation: `artifact_sha256` is exactly 64 `[0-9a-f]` (byte comparison across repositories makes
   case significant), `pooling`/`normalization` non-empty, `max_input_tokens >= 1`. Pooling and normalization stay
   free-form like `model`; the ADR pins `last` and `l2`. MCP and CLI both call it; decoders trust the database.
3. **Absent ≠ declared, both directions.** An unserved store under a served configuration, and the reverse, is
   `EmbeddingModelMismatch` and the server refuses to start. An unserved store under an unserved configuration still
   starts. Nothing is back-filled: nobody knows which file produced old vectors. Recoveries an operator can run
   today: drop the declaration (keep serving the recorded identity) or start blank (production's path).
   Reconstruction (ADR §4) is the normative path but is library-only (`MemoryEngine::reconstruct`) until #689 ships
   its CLI/MCP surface; the docs and PR say so rather than implying an operator command.
4. **Default instruction in the provider, unconditional.** `HttpEmbeddingProvider.query_instruction` becomes a
   `String` initialised to `DEFAULT_QUERY_INSTRUCTION`; `with_query_instruction("")` means none. The instruction belongs
   to ME's store, not to a model, so it is not gated on a model slug. Consequence (breaking, `!` commit): a symmetric
   model's queries gain the prefix until its operator sets `query_instruction = ""`; stored vectors and identity are
   unaffected. MCP and CLI keep `query_instruction` as an override (absent = pinned default).
5. **Configuration.** MCP `[embedding]` gains four flat optional keys named as the fields; flags
   `--embed-artifact-sha256`, `--embed-pooling`, `--embed-normalization`, `--embed-max-input-tokens` with
   `MEMORY_MCP_EMBED_*` env override TOML per field, then `from_parts`. The whole CLI-over-TOML embedder merge
   (url, model, key, provider, query instruction, served fields) moves from the binary's `build_embedder` into one
   library function, `config::resolve_embedding(&EmbeddingOverrides, Option<&EmbeddingSection>, engine_dim: usize)
   -> Result<Option<ResolvedEmbedding>>` (`engine_dim` is the resolved engine dimension, today's `native_dim`
   fallback when no `[embedding]` section exists). It also owns the two pure dimension checks now in `build_embedder`
   (`mrl_dim == engine_dim`; `native_dim == engine_dim` without MRL) and returns `memory_engine::error::Result`
   (`ConflictError::PolicyParameter` for its own rules, so `from_parts` composes with `?`). It runs before the engine
   opens, and the providers are built from its result before the engine opens too (construction never contacts the
   endpoint; `with_mrl_dim`'s range check stays where it is), so a configuration refusal creates no database file and
   migrates nothing. Its errors print as `invalid embedding configuration: {e}`, so the precedence rule has one home and `tests/config_merge.rs` tests it
   instead of its simulated copy (`merge_embedding_config`, deleted with the tests that only exercised it). MCP also gains `--embed-dim` /
   `MEMORY_MCP_EMBED_DIM` (overrides `[engine] embed_dim`), so an environment-only deployment can start on a blank
   store, which today fails with `cannot determine embed_dim`, and `--embed-query-instruction` /
   `MEMORY_MCP_EMBED_QUERY_INSTRUCTION`, so such a deployment can opt out of Decision 4's default. The CLI's `EmbeddingArgs` gains the same four flags
   with `MEMORY_ENGINE_EMBED_*` env (#619 mirroring). No defaults: an absent declaration is "no served artifact".
6. **Migration v14 → v15.** Four nullable columns `artifact_sha256 TEXT, pooling TEXT, normalization TEXT,
   max_input_tokens INTEGER`, no DEFAULT, no CHECK, added with `ALTER TABLE … ADD COLUMN` each only if
   `pragma_table_info` lacks it (`schema/tests.rs:505`, `:792`, `simulate_v13` and the CLI's `create_stale_db` in
   `cli_integration.rs` re-run migrations on a current schema). No table rebuild (it would cascade-delete `fact_vectors`). Decode: all NULL → `None`, all set → `Some`,
   partial → `MemoryError::Internal`. Fresh DDL appends the same columns after `created_at`. No `.pak` bump.
7. **Postgres.** List the columns on #634; PG schema unchanged. `load_fingerprint` returns `served: None`;
   `store_embedding_fingerprint`, `record_embedding_fingerprint_if_absent` and `check_embedding_compatible` first call
   `ensure_no_served`, returning `MemoryError::NotImplemented` naming dutiona/memory-engine#634, so PG never drops the
   artifact silently. The live PG suite runs as the parity proof.
8. **Pre-computed embeddings.** Today a pre-computed `memory_add_fact` on a blank store records whatever the caller
   declares (`tools/parse.rs` builds it without `served`; `engine/ingest.rs` `record_if_absent`). Under a served
   configuration that would stamp the store unserved and the server would refuse its own next start. Fix: when the
   server has a configured embedder, the pre-computed branch of `handle_add_fact` requires `declared ==
   embedder.fingerprint()` before anything is written, else an MCP `invalid_params` error naming both identities
   (`EmbeddingModelMismatch`'s text speaks of a stored identity, which is false on a blank store). A served
   deployment therefore rejects every pre-computed write, blank or stamped. `memory_query`'s pre-computed branch
   stamps nothing and is unchanged in code, but on a served store its declaration (no `served`) no longer matches,
   so it is refused too; the docs and PR say so. Adding four served tool parameters is deferred: #1020 removes these
   tools from the model surface; recorded in the PR body.
9. **Which incidental fixes ride along.** AGENTS.md "Collateral issues" keeps *unrelated* problems out of the PR;
   Michaël's boy-scout rule puts rot fixes in their own commits. Items 2–10 and 19 each touch the identity surface
   this issue changes (the issue's own boy-scout, the docs of functions edited here, the parity-proof command, the
   migration facts item 13 relies on, reconstruction as the ADR's recovery path, the debug-build panic that blocks
   the acceptance test, the CLI half of the identity check, a startup that skips the check), so they ride as separate
   commits. Unrelated findings (CLI `add-fact --embedding`'s sentinel identity, `.pak` archives carrying no identity,
   `memory_apply_cycle_report` accepting vectors with no declared identity, `build_summary_generator` silently
   ignoring a half-configured summary endpoint) go to the collateral ledger with an issue recommendation.

## Implementation sequence

Branch commits: Conventional Commits, imperative, body says why, no co-author line. Each boy-scout fix is its own
commit. All commands run unpiped from the worktree root.

- [ ] **1. Commit the plan.** File: this plan. No behaviour. The worktree `.worktrees/feat-1017-embedding-identity`
  already exists and is locked. Commit: `docs(plans): plan the served embedding identity (#1017)`.

- [ ] **2. Boy-scout (issue): `--mrl-dim` help names `--native-dim`.** File:
  `crates/memory-engine-cli/src/commands/embedding_args.rs:54`. No test (help text). Check: `git grep -n --
  '--embed-dimensions' crates` prints nothing. Commit: `docs(cli): point --mrl-dim at --native-dim, the real flag`.

- [ ] **3. Boy-scout: `build_embedder`'s doc comment sits on `KNOWN_PROVIDERS`.** File:
  `crates/memory-engine-mcp/src/main.rs:123-129`: move the four `/// Build the embedding provider…` lines above `fn
  build_embedder`. Check: `cargo clippy -p memory-engine-mcp --all-targets --all-features -- -D warnings`. Commit:
  `docs(mcp): put build_embedder's doc comment back on build_embedder`.

- [ ] **4. Boy-scout: `probe_embed_dim`'s doc.** File: `crates/memory-engine-mcp/src/config.rs`: the doc states the real
  order (active `embedding_spaces` row, then legacy `embedding_meta`, then bare `embed_dim`). Commit:
  `docs(mcp): describe probe_embed_dim's registry-first lookup`.

- [ ] **5. Boy-scout, verify first: the live-Postgres command.** Files: `crates/me-backend-postgres/src/{tests.rs:13,lib.rs:56}`,
  `CLAUDE.md:40`. Run the documented `cargo test -p memory-engine --features backend-postgres,test-util -- --ignored`
  and `cargo test -p me-backend-postgres --features test-util -- --ignored` with Docker. Only if the first runs none
  of the `tests.rs` cases, all three sites name the second. Otherwise drop the item and say so in the PR. Commit:
  `docs(postgres): name the command that runs the live Postgres suite`.

- [ ] **6. Boy-scout, verify first: the schema-test docs on `ADD COLUMN`.** File:
  `crates/me-backend-sqlite/src/store/schema/tests.rs` (docs of `schema_ddl_migration_chain_snapshot` and
  `migration_chain_ddl_differs_from_init_known_artifact`). If the v11 migration-chain snapshot shows ADD-COLUMN columns
  inside `CREATE TABLE` text while the comments say `ADD COLUMN` leaves the text unchanged, correct the comments.
  Commit: `docs(storage): correct why migrated and fresh schema DDL text differ`.

- [ ] **7. Boy-scout: reconstruction space names collide for distinct identities.** File:
  `crates/memory-engine/src/engine/reconstruct.rs` (`space_name_for`, whose doc claims distinct identities get
  distinct names). Today the name is the sanitized `model_provider_dim`, so an MRL or element-type difference
  collides with the plain identity of the same model, and `begin_populating` refuses the second space. Red: unit
  tests `space_name_for_distinguishes_mrl_and_element_type` (those pairs differ; the same fingerprint gives the same
  name) and `space_name_for_keeps_the_legacy_name_for_a_plain_identity`; `cargo test -p memory-engine
  --all-features space_name_for` fails on the first. Change: an identity is *plain* iff `*fp ==
  EmbeddingFingerprint::new(fp.model.clone(), fp.provider.clone(), fp.dim)` (so item 12's `served` field joins the
  predicate by itself); a plain identity keeps today's name exactly, so a reconstruction an older binary left
  `populating` resumes after the upgrade; any other identity gets the slug, `_`, and the first 16 hex of
  `blake3(serde_json::to_vec(fp))` (both already dependencies of `memory-engine`). `space_name_for` becomes
  fallible (`-> Result<String>`; the serialization error converts through `MemoryError::Serialization`), since the
  workspace denies `unwrap`. Residuals, stated in the doc and accepted: an older binary's in-flight non-plain
  reconstruction restarts under the new name (re-embedding, its old `populating` row left for the operator to
  deprecate); sanitization collisions between plain slugs such as `a.b` and `a_b` remain as today. Also a facade
  test `reconstruct_resumes_a_legacy_named_populating_space`: a plain-target `populating` space opened under the
  hard-coded literal of today's slug (not through `space_name_for`) and partly backfilled is resumed, not
  duplicated; `reconstruct_resumes_a_crashed_reconstruction` gets the one-line update for the `Result` return. The served-artifact pair is tested in item 15, once the
  field exists. Green: same command plus `cargo test -p memory-engine --all-features reconstruct`. Commit:
  `fix(engine): give MRL and element type a say in reconstruction space names`.

- [ ] **8. Boy-scout bug: `reconstruct` checks only the target's dim against its embedder.** File:
  `crates/memory-engine/src/engine/reconstruct.rs:126-134`. A caller can declare identity B while re-embedding with
  a provider that reports A; the vectors are then stamped B and every later check accepts them. Red:
  `reconstruct_refuses_a_target_its_embedder_does_not_report` (same dim, different model);
  `cargo test -p memory-engine --all-features reconstruct_refuses` fails (it promotes). Change: after
  the dim check, require `new_fingerprint == &embedder.fingerprint()`, else
  `MemoryError::Conflict(ConflictError::PolicyParameter(..))` naming both identities (`EmbeddingModelMismatch`'s
  "store was created with" text would be false here); the read-only check and the dim check stay first, so their
  errors and the #972 ordering are unchanged; `reconstruct`'s `# Errors` documents the new case. Every existing
  reconstruct test's target already equals its embedder's identity, so none changes. Green: `cargo test -p
  memory-engine --all-features reconstruct` and `cargo test -p memory-engine --all-features --test read_only_test`. Commit: `fix(engine): reconstruct refuses a target identity its embedder does
  not report`.

- [ ] **9. Boy-scout bug: blocking HTTP clients are built on the async executor.** In debug builds reqwest 0.12's
  blocking client creates and drops a throwaway runtime on the calling thread (`reqwest` `src/blocking/wait.rs`,
  `enter()`, verified in the locked 0.12.28 source); dropping it inside `block_on` panics ("Cannot drop a runtime in
  a context where blocking is not allowed"). `crates/memory-engine-cli/src/commands/consolidate.rs:118-123` already
  works around it. Release builds compile the check out, and no test exercises these paths, so nothing has seen it;
  every process test of this plan runs a debug binary. Files: `crates/memory-engine-mcp/src/main.rs`
  (`build_embedder`, `build_summary_generator`), `crates/memory-engine-cli/src/commands/{query.rs,batch_ingest.rs,bootstrap.rs}`,
  `crates/memory-engine-mcp/tests/identity_startup.rs` (new; item 18 extends it), `crates/memory-engine-cli/tests/cli_integration.rs`.
  Red:
  - `identity_startup.rs` starts the harness: `stamped_store(fp)` stamps a temp database through
    `MemoryEngine::builder(fp.dim)` + one `add_fact` with a stub provider reporting `fp`, then drops the engine; the
    config TOML is written through `toml` serialization so paths are quoted; `run_server(config, envs)` spawns
    `env!("CARGO_BIN_EXE_memory-engine-mcp")` with every `MEMORY_MCP_*` and `RUST_LOG` removed and stdio piped, and the
    **serving** flow (stdio-specific; #1018 rewrites it): write a stdio `initialize`, read stdout on a reader thread
    with a 60 s `recv_timeout` until the JSON-RPC response for `id` 1, close stdin, and assert exit code 0 (first
    observable in green; the red run exits 101). Item 18 adds the **refusing** flow with its first cases, so no
    helper is dead code at any commit (no `#[allow(dead_code)]`). No path can hang the workspace test run. First case,
    `serves_with_an_embedder_configured` (an unserved store under a matching unserved config with an embedder,
    endpoint `http://127.0.0.1:9/v1/embeddings`, never contacted before serving): today exit 101 with the panic.
  - CLI, each with `--embed-url http://127.0.0.1:9/v1/embeddings` and the test store's identity (`--embed-model mock
    --embed-provider test` against `create_test_db`, so item 10's check passes later): `query` fails with `query
    embedding failed`; `batch-ingest` with one valid line fails with `no facts ingested`; `bootstrap --memory-dir`
    over one `.md` file fails with its embed error. Each asserts no `panicked` in stderr; today all three panic.
  - `cargo test -p memory-engine-mcp --all-features --test identity_startup` and `cargo test -p memory-engine-cli
    --all-features --test cli_integration unreachable_embedder` fail.
  - Change: construct the providers (and, in `query`, call `embed_query`) inside `tokio::task::spawn_blocking` with
    owned inputs, the `consolidate.rs` pattern. A blocking client's drop only joins its own thread, so drops stay
    where they are. #1018 must keep this when it rebases `main.rs`.
  - Green: both commands. Commit: `fix: build blocking HTTP clients off the async executor`.

- [ ] **10. Boy-scout bug: CLI `query` never checks the embedder identity.** Files:
  `crates/memory-engine-cli/src/commands/query.rs:103-108`, `crates/memory-engine-cli/tests/cli_integration.rs`.
  `docs/reference/cli-inspector.md` already claims the #614 check. Red: `query_refuses_an_embedder_whose_identity_differs`
  (store from `create_test_db`, `--embed-model other --embed-url http://127.0.0.1:9/v1/embeddings`) expects stderr
  `embedding identity check failed`; after item 9 it fails later with `query embedding failed` (connection refused). Change:
  `engine.verify_embedding_identity(&provider).await`, its error wrapped as `embedding identity check failed: {e}`
  (MCP `main.rs` wording), before the (item 9: off-executor) `embed_query`. Green: `cargo test -p memory-engine-cli
  --all-features --test cli_integration query_refuses`. Commit: `fix(cli): check the embedder identity before
  embedding a query`.

- [ ] **11. ADR-0015 amendment (carries the APPROVAL POINT).** File:
  `docs/design/adr/0015-cross-layer-embedding-identity-policy.md`. No executable behaviour. Normative content:
  - Status: `Accepted (2026-06-16); amended 2026-11-16 (dutiona/memory-engine#1017)`. Context gains one paragraph: a
    slug alone does not pin a vector space; another file (quantization), pooling, normalization or input limit under
    the same slug gives other vectors.
  - §1: `provider` examples become `llama.cpp`, `tei`, `ollama`, `openai`, `onnx`. Four rows after `element_type`:
    `artifact_sha256` (string | null: SHA-256, 64 lowercase hex, of the single model file the provider serves; for
    llama.cpp the GGUF; operator-declared), `pooling` (string | null, e.g. `last`), `normalization` (string | null,
    e.g. `l2`), `max_input_tokens` (integer | null). Rule: these four are the **served artifact**; a store records
    all four or none. Each store records one tuple per embedding space, exactly one space active; the Memory layer's
    single space is the degenerate case.
  - §2: the served artifact is part of the first-write stamp; a blank store records whatever its first write
    declares, so a deployment declares every field, and a store refuses a pre-computed first write whose declared
    identity differs from its configured provider's.
  - §3: an absent served artifact equals only another absent one; each store checks at startup and MUST refuse to
    start on a mismatch (was RECOMMENDED); a pre-computed declaration that omits a field the store records does not
    match.
  - §4: changing any served field, or adopting one on a store recorded without it, is a reconstruction, and a
    reconstruction re-embeds with a provider that reports exactly the target identity. Mechanism split into **Memory
    layer** (the current paragraph, references qualified, the #624 index rebuild stated as done before `reconstruct`
    returns) and **Knowledge layer** (per-space sqlite-vec tables: create → backfill → promote → deprecate → cleanup,
    registry `embed_spaces`, `src/knowledge_base/embed_swap.py`, same atomic-swap semantics). The "do not edit the KB
    copy" blockquote goes. No line numbers anywhere in the ADR (it is copied byte-identical across repositories).
  - New §5 **Pinned identity (both layers)**: `model` `Qwen/Qwen3-Embedding-0.6B`, `provider` `llama.cpp`, `dim`
    `1024`, `matryoshka_base_dim` null, `element_type` `float32`, `artifact_sha256`
    `06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439`, `pooling` `last`, `normalization` `l2`,
    `max_input_tokens` `2048`; then the file name, size, repository and commit, and the 2,048 rationale with
    dutiona/infra#77.
  - New §6 **Query instructions**: asymmetric models embed queries with an instruction and documents without one;
    each store pins its own, applied automatically by its client; documents never carry one, so document vectors stay
    comparable across stores; the instruction is policy, not identity (not recorded, changing it re-embeds nothing).
    The section defines its own notation: `\n` is one line feed (U+000A), never the two characters `\` `n`, and
    each string ends with one space after `Query:`. Table: Memory layer `Instruct: Given a search query, retrieve
    relevant memory facts.\nQuery: `; Knowledge layer `Instruct: Given a search query, retrieve relevant document
    passages.\nQuery: `.
  - Consequences: the "Slug-level identity, not weight-level" bullet is rewritten (not kept beside the new one) as
    "declared, not probed" (`artifact_sha256` is checked at install, a canary-vector check deferred); both layers
    enforce the policy; schema change without data migration (old stores stay unserved and refuse a served
    configuration); truncation to `max_input_tokens` is outside this ADR.
  - Every issue reference reads `dutiona/<repo>#N`, because the KB copy is byte-identical.
  - Check: `git grep -nE '(^|[^a-z/-])#[0-9]+' docs/design/adr/0015-cross-layer-embedding-identity-policy.md` and
    `git grep -nE '\.rs:[0-9]' docs/design/adr/0015-cross-layer-embedding-identity-policy.md` print nothing;
    `uv run sphinx-build -b html docs docs/_build` adds no warning for the file.
  - Commit: `docs(adr): pin the served embedding identity and per-store query instructions`.

- [ ] **12. Types: the served artifact.** Files: `crates/me-types/src/types/cognitive.rs`,
  `crates/memory-engine/src/lib.rs` (re-export `ServedArtifact`, extend `reexports_are_accessible` if it lists the
  identity types), the two struct-literal sites (`crates/me-backend-sqlite/src/store/embedding_spaces.rs:67`,
  `crates/me-backend-postgres/src/schema.rs:258`, set `served: None` until items 13/14). Red (`cognitive.rs` tests):
  equality differs on `None` vs `Some` and on each served field; an unserved fingerprint serializes to exactly the
  five ADR keys; five-key JSON reads as `served: None`; an `EmbeddingSpaceSnapshot` round-trips a served fingerprint
  through `#[serde(flatten)]` and rejects an unknown key in `served`; `from_parts` returns `Ok(None)`, `Ok(Some)`,
  and `Conflict(PolicyParameter)` naming the missing fields, for an uppercase or 63-char sha, and for
  `max_input_tokens = 0`. `cargo test -p me-types --all-features` does not compile. Change: Decisions 1–2; the
  "Cross-layer parity contract" rustdoc lists all nine normative names. Green: same command, plus `cargo build
  --workspace --all-features`. Commit: `feat(types): add the served artifact to the embedding fingerprint`.

- [ ] **13. SQLite: schema v15 stores the served artifact.** Files:
  `crates/me-backend-sqlite/src/store/schema/{ddl.rs,migrations.rs,mod.rs,tests.rs}`, the schema and dump `.snap`
  goldens, `crates/me-backend-sqlite/src/store/{embedding_spaces.rs,embedding_meta.rs}`,
  `crates/memory-engine/src/inspect/restore.rs` (a comment hard-coding `"14"`, and a test),
  `crates/memory-engine-cli/tests/cli_integration.rs` (a comment), `docs/design/schema-evolution-policy.md`. Red:
  - `embedding_spaces.rs`: served round-trip; upsert served → unserved → served, through both the UPDATE and the
    INSERT branch; a raw row with only `pooling` set makes `find_active` return `Internal`; `begin_populating`
    resumes only with the same served artifact.
  - `embedding_meta.rs`: `record_if_absent` and `check_compatible` mismatch in both directions; an unserved store
    accepts an unserved candidate.
  - `schema/tests.rs`: a new `simulate_v14` (mirroring `simulate_v13`: drop the four columns with `ALTER TABLE …
    DROP COLUMN`, set the version to 14) proves the single v14 → v15 step keeps a recorded identity with the four
    columns NULL; the migration is idempotent on a fresh current schema (the `:505`/`:792` roll-back tests);
    version literals `"14"` become `CURRENT_SCHEMA_VERSION`; the stale chain comments at `:513` ("v11→v12→v13") and
    `:576` ("v12→v13→v14") name v15; `create_stale_db`'s idempotence comment (`cli_integration.rs:542-545`) names the
    `pragma_table_info` guard.
  - The existing `fresh_vs_migrated_embedding_spaces_converge` (`schema/tests.rs:2297`) compares normalized SQL text,
    which `ADD COLUMN` rewrites differently from the fresh DDL: it changes to compare the table's columns on `PRAGMA
    table_xinfo` (name, type, notnull, default, pk), keeps the text comparison for `idx_embedding_spaces_one_active`,
    and gains an explicit assertion that the `CHECK(status IN ('active','populating','deprecated'))` enumeration is
    present in both (table_xinfo cannot see it).
  - The inline golden of `schema_ddl_snapshot_is_stable` (`schema/tests.rs:1540`, not an insta snapshot) is edited by
    hand for the four columns.
  - facade: JSON dump then restore preserves a served active space and a served populating space (in
    `crates/memory-engine/src/inspect/restore.rs` tests); `first_write_records_the_served_artifact` in
    `crates/memory-engine/src/engine/tests.rs`: on a blank store, one `add_fact` through a provider whose fingerprint
    carries a served artifact records all four fields (`engine.storage().load_embedding_fingerprint()` reads them
    back), the first-write proof item 18 relies on.
  - `cargo test -p me-backend-sqlite --all-features`, `cargo test -p memory-engine --all-features inspect` and
    `cargo test -p memory-engine --all-features first_write_records_the_served_artifact` fail.
  - Change: Decision 6; `CURRENT_SCHEMA_VERSION = 15`; `MIGRATIONS` gains `(migrate_v14_to_v15, false)`; `SELECT_COLS`
    and the three writers handle the four columns through `served_to_sql` beside `dims_to_sql`; `ensure_match`'s doc
    names them. Insta snapshots are re-accepted with `INSTA_UPDATE=always`; the goldens (the `.snap` files and the
    inline `schema_ddl_snapshot_is_stable` string) change only in the `embedding_spaces` columns and
    `schema_version` 15. The policy doc gains an "additive columns" paragraph and the Postgres lag. If
    another branch lands a v15 first, renumber on rebase.
  - Green: the three commands pass. Commit: `feat(storage): store the served artifact with the embedding space (schema v15)`.

- [ ] **14. Postgres: refuse a served fingerprint until #634.** Files: `crates/me-backend-postgres/src/schema.rs`
  (module doc names the gap). Red: a plain `#[cfg(test)]` unit test of `ensure_no_served` (no Docker); `cargo test -p
  me-backend-postgres --features test-util ensure_no_served` does not compile. Change: Decision 7. Green: the unit test;
  then the live suite (item 5's verified command) passes with `every_table_has_its_full_v14_column_set` unchanged.
  That proves Postgres is unchanged, not that it matches SQLite: it deliberately lags by four columns, held closed by
  `ensure_no_served`; the PR states this as the "why the parity proof doesn't cover v15" branch of the acceptance.
  Operations: `gh issue comment 634 --repo dutiona/memory-engine --body-file <scratch>` listing the four columns
  (SQLite `TEXT, TEXT, TEXT, INTEGER`; PG `BIGINT` for the last), nullable, all-or-none; the PG work (a v1 → v2 step,
  load/store, removing the guard, extending the parity manifest). Commit: `feat(storage): Postgres refuses a served
  fingerprint until #634 stores it`.

- [ ] **15. Reconstruction across served artifacts (tests only).** File:
  `crates/memory-engine/src/engine/reconstruct.rs` (tests). Red: none expected once items 7, 8 and 13 have landed
  (item 7's plain predicate already counts `served`); the tests are the proof. `space_name_for_distinguishes_served_artifacts`
  (two fingerprints that differ only in `artifact_sha256` get different names, neither the legacy slug) and
  `reconstruct_twice_across_served_artifacts`: stamp A via `add_fact`, reconstruct to B then C that differ from A
  only in `artifact_sha256` (each with an embedder reporting its target); each outcome and the active identity equal
  the target, `served` included. Command: `cargo test -p memory-engine --all-features served_artifacts`. Commit:
  `test(engine): reconstruct across served artifacts`.

- [ ] **16. Embed: the provider declares a served artifact.** File: `crates/memory-engine-embed/src/http.rs`. Red:
  `fingerprint_reports_declared_served_artifact`, with and without MRL; `cargo test -p memory-engine-embed --lib`
  does not compile. Change: field `served: Option<ServedArtifact>`, `#[must_use] pub fn with_served_artifact(self,
  ServedArtifact) -> Self`, documented as taking a value the caller obtained from `ServedArtifact::from_parts` (the
  fields are public, so the type does not enforce it); `fingerprint()` attaches it. Green: same command. Commit:
  `feat(embed): carry the declared served artifact in the provider fingerprint`.

- [ ] **17. Embed: queries carry ME's pinned instruction by default.** Files:
  `crates/memory-engine-embed/src/{http.rs,lib.rs}`, `crates/memory-engine-embed/tests/{asymmetric_embedding.rs,qwen_integration.rs}`,
  `crates/memory-engine-mcp/tests/embedding_integration.rs`, field docs in `crates/memory-engine-mcp/src/config.rs` and
  `crates/memory-engine-cli/src/commands/embedding_args.rs`. Red (wiremock, asserting each request body's `input`,
  provider built by `new` alone): `embed_query` sends `"Instruct: Given a search query, retrieve relevant memory
  facts.\nQuery: hello"`; `embed_query_batch` prefixes each; `embed`/`embed_batch` send raw text; `""` sends queries
  bare; a unit test pins `DEFAULT_QUERY_INSTRUCTION` byte for byte; MCP `query_path_applies_pinned_instruction_by_default`
  and `add_fact_path_never_applies_pinned_instruction`. `cargo test -p memory-engine-embed --test asymmetric_embedding`
  fails on the default cases. Change: Decision 4; the two `option_if_let_else` allows on `embed_query*` go;
  `qwen_integration.rs`'s symmetric `provider(false)` opts out with `""`; the lib quick start (nomic) carries
  `.with_query_instruction("")` explicitly; the `HttpEmbeddingProvider` rustdoc that calls the instruction opt-in
  (the "Asymmetric embedding" section, the "Symmetric (unchanged)" example, `with_query_instruction`'s "defaults to
  `None`") is rewritten; the constant's doc cites ADR-0015 §6. Green: `cargo test -p
  memory-engine-embed --all-features` and `cargo test -p memory-engine-mcp --all-features --test
  embedding_integration`. Commit: `feat(embed)!: prefix queries with ME's pinned instruction by default` (body: how a
  symmetric model opts out).

- [ ] **18. MCP: declare the served artifact, know `llama.cpp`, refuse per field (process test).** Files:
  `crates/memory-engine-mcp/src/{config.rs,main.rs}`, `crates/memory-engine-mcp/tests/{config_merge.rs,identity_startup.rs}`.
  Red:
  - `config_merge.rs` exercises real library code: the four keys parse; new `EmbeddingSection` literals carry the
    four served keys; `config::resolve_embedding` (Decision 5; `EmbeddingOverrides` holds the CLI/env `Option`s for url,
    model, key, provider, query instruction and the four served fields; `ResolvedEmbedding` carries the merged
    values plus `native_dim`, `mrl_dim`, `timeout_secs` and `served: Option<ServedArtifact>`) takes each field from the
    override, else from TOML, then `from_parts`. Tested there: per-field override for every field, the
    query-instruction override including `Some("")`, the partial served declaration naming the missing fields, and
    `Ok(None)` with neither url nor model, and the no-TOML case (overrides only) whose `native_dim` falls back to
    `engine_dim`, and the two dimension checks. The simulated `merge_embedding_config`, the tests that only
    exercised it, and `cli_db_path_overrides_toml` (same anti-pattern) are deleted.
  - `identity_startup.rs` (item 9's harness; `tempfile` and `tokio` are dev-dependencies and `toml` a normal one,
    all present; no new dependency) gains the **refusing** flow (stdio-specific): write `initialize` (a `BrokenPipe`
    because the child already exited is tolerated), keep stdin open, wait up to 60 s for the process to exit on its
    own (kill and fail on timeout). BASE = the §5 tuple, with `[engine] embed_dim = 1024` (so the `dim` case reaches the open-time refusal
    rather than the `dimensions` check).
  - Thirteen refusals, each on the refusing flow, asserting: non-zero exit, no JSON-RPC response on stdout (empty
    stdout), the stderr marker, and an unchanged store record. Cases: one per field (`model`,
    `provider`, `matryoshka_base_dim`, `element_type`, `artifact_sha256`, `pooling`, `normalization`,
    `max_input_tokens`: `embedding identity check failed`; `dim`: `failed to open engine`); an unserved store under
    BASE; a served store under BASE minus the served keys; an env override of `artifact_sha256` over a BASE TOML
    (proves the real override path end to end); a partial env declaration (`MEMORY_MCP_EMBED_ARTIFACT_SHA256` only,
    TOML without served keys) on a blank path: `invalid embedding configuration` naming the three missing fields, and
    no database file created.
  - Three more controls on the serving flow (item 9's unserved control is the fourth): a BASE store under BASE,
    whose stderr also lacks `is not one of` (proves `llama.cpp` is known); an environment-only deployment
    (`--db-path` plus `MEMORY_MCP_EMBED_*` for url, model, provider, dim and the four served fields, no TOML) on a
    BASE store, which proves the env values reach the identity check; the same on a blank store, which proves
    `MEMORY_MCP_EMBED_DIM`. Recording on the first write is proven at the engine seam (item 13's blank-store
    `add_fact` test), not here. The `initialize` exchange is the only transport-specific code; #1018 swaps it for one
    HTTP request.
  - `cargo test -p memory-engine-mcp --all-features --test identity_startup --test config_merge` fails (unknown keys,
    no env, `llama.cpp` warning).
  - Change: Decision 5; `KNOWN_PROVIDERS` gains `llama.cpp` and the `provider`/`--embed-provider` docs list it;
    `main.rs` keeps clap (its embedder flags flatten into `EmbeddingOverrides`, gaining the four `--embed-*` served
    flags and `--embed-query-instruction` / `MEMORY_MCP_EMBED_QUERY_INSTRUCTION`, so a flag- or env-only symmetric
    deployment can opt out with `""`), calls `resolve_embedding` right after resolving `embed_dim`, builds the
    providers from `ResolvedEmbedding` (in item 9's `spawn_blocking`; `with_served_artifact` when `served` is set),
    and only then calls `MemoryEngine::builder(..).build()`; the stale "handled in build_embedder()" note and step
    comments in `main.rs` are rewritten. `EmbeddingOverrides` and `ResolvedEmbedding` hold the API key, so neither
    derives `Debug` (the `EmbeddingArgs` precedent); tests use `.err().expect(..)`; `--embed-dim` /
    `MEMORY_MCP_EMBED_DIM` overrides `[engine] embed_dim` in `load_config`.
  - Green: same command; `identity_startup` reports all seventeen cases run (build green is not test green). Commit:
    `feat(mcp): declare the served artifact and refuse to start on any identity mismatch`.

- [ ] **19. Boy-scout bug: MCP serves with no identity check on a half-configured embedder.** `build_embedder`
  returns `None` when only one of url and model is set (`crates/memory-engine-mcp/src/main.rs:162-164`), so an
  environment-only deployment that misses one variable serves with no embedder and no identity check; the CLI
  already rejects the same input. Any other embedder field (provider, key, query instruction, served fields) set
  without url and model is silently dropped the same way. Files: `crates/memory-engine-mcp/src/config.rs`
  (`resolve_embedding`), `crates/memory-engine-mcp/tests/{config_merge.rs,identity_startup.rs}`. Red: `config_merge.rs`
  unit cases (url without model, model without url, provider alone, served fields alone) and two refusing-flow
  process cases on a BASE store with a TOML-less env configuration that sets `MEMORY_MCP_EMBED_DIM` (so the exit
  cannot come from `cannot determine embed_dim`): `refuses_a_half_configured_embedder` (model without url; marker
  `invalid embedding configuration` plus `url`) and `refuses_embedder_fields_without_an_embedder` (served fields
  only; same prefix plus `url`); today they resolve to "no embedder" and serve. Change: one rule in
  `resolve_embedding`: any embedder field set without both url and model is an error naming url and model. Green:
  `cargo test -p memory-engine-mcp --all-features --test config_merge --test identity_startup` (nineteen process
  cases). Commit: `fix(mcp): refuse a half-configured embedder instead of serving without an identity check`.

- [ ] **20. MCP: a pre-computed write must match the configured provider.** Files:
  `crates/memory-engine-mcp/src/tools/handlers/p0.rs` (`handle_add_fact`'s pre-computed branch),
  `crates/memory-engine-mcp/tests/embedding_integration.rs`. Red: `precomputed_add_fact_on_blank_store_rejected_under_served_config`
  (blank store, served provider configured, a pre-computed `memory_add_fact`: error, and no `embedding_spaces` row
  afterwards) and `precomputed_add_fact_matching_configured_provider_is_accepted` (no served artifact, declared ==
  configured); `cargo test -p memory-engine-mcp --all-features --test embedding_integration precomputed` fails on the
  first (it stamps). Change: Decision 8. Green: same command. Commit: `fix(mcp): a pre-computed fact must declare the
  configured provider's identity`.

- [ ] **21. CLI: served-artifact flags.** Files: `crates/memory-engine-cli/src/commands/{embedding_args.rs,bootstrap.rs,batch_ingest.rs}`
  (test literals). Red: all four flags reach `fingerprint().served`; a partial set errors naming the missing fields; a
  non-canonical sha errors. `cargo test -p memory-engine-cli --all-features embedding_args` does not compile. Change:
  a flattened `ServedArgs` (`clap::Args, Clone, Default`) with the four `--embed-*` flags and `MEMORY_ENGINE_EMBED_*`
  env; `build_inner` applies `from_parts` then `with_served_artifact`; `build_optional` refuses served flags set
  without both `--embed-url` and `--embed-model` (mirroring item 19 for the new group; `--embed-provider` has a clap
  default and cannot take part), with a red test; the module doc lists the keys mirrored from MCP; `--embed-provider`
  lists `llama.cpp`. Green: `cargo test -p memory-engine-cli --all-features`. Commit:
  `feat(cli): declare the served artifact with --embed-* flags`.

- [ ] **22. Reference docs.** Files: `docs/reference/mcp-server.md`, `docs/reference/cli-inspector.md`,
  `docs/advanced/reconstruction.md`, `docs/design/embedding-identity-and-tei-qwen-migration.md` (status line). Content
  under Documentation. Check: `uv run sphinx-build -b html docs docs/_build` adds no warning. Commit: `docs: configure
  the served artifact and the default query instruction`.

- [ ] **23. Ship (operations).** Run Verification at the final clean HEAD. Open the PR from
  `feat/1017-embedding-identity`: `Refs dutiona/infra#37` and `Closes #1017` (never a closing keyword beside any other
  number); `Classification: Feature (researched) (user-confirmed)`, execution path, planning tier and review gate
  lines; the APPROVAL POINT block; the behaviour changes, flagged for the squash message (the title keeps `!`, the
  body names both breaks): the default instruction and its `""` opt-out, and schema v15 (migrated on open with no
  pre-migration backup until #1018 wires `backup_dir`; older binaries refuse a v15 store); the pre-computed and CLI
  `query` refusals; item 19's refusal of a half-configured embedder (a configuration that served FTS-only now exits
  non-zero); that `max_input_tokens = 2048` rests on dutiona/infra#77 and changing it after any store is stamped
  is a reconstruction in both layers, so #77 must confirm it before the deployment's first write; the Postgres listing and the parity-proof output with its "unchanged, not matching" reading; the
  deferred pre-computed tool parameters; Decision 9's rule for the incidental commits; the #1018 rebase note;
  `Collateral findings`; the gate output. Then `/super-review` (Full gate, persisted route) and `/finish-pr`; merge
  stays with Michaël.

## Documentation

- ADR-0015 (item 11): the normative contract and the approval point.
- `docs/reference/mcp-server.md`: the four `[embedding]` keys, flags and env; the flag-only nomic invocation gains
  `--embed-query-instruction ''`; `MEMORY_MCP_EMBED_DIM`; `llama.cpp`; a
  pinned-identity TOML example with the §5 values; the default instruction and the `""` opt-out, with the symmetric
  (nomic) example carrying `query_instruction = ""` explicitly; the startup check covering served fields, the
  legacy-store refusal and its recoveries (drop the declaration or start blank; reconstruction is library-only until
  #689), noting that a refused start has already migrated the store to v15, so recovery uses the new binary and
  rolling back is not one of them; the pre-computed `memory_add_fact` refusal under a configured provider, and `memory_query` with a
  pre-computed vector refused on a served store; `--embed-query-instruction` / `MEMORY_MCP_EMBED_QUERY_INSTRUCTION`;
  the half-configured-embedder refusal.
- `docs/reference/cli-inspector.md`: the four flags and env; the default instruction, with the symmetric example
  carrying `--query-instruction ''`; drop the `--query-instruction "…\nQuery: "` example (a shell passes a literal
  backslash-n, so it never matched); the `query` identity check.
- `docs/advanced/reconstruction.md`: served-artifact changes are reconstructions; the target must equal the
  embedder's identity; the digest-based space name.
- `docs/design/schema-evolution-policy.md`: v15's additive columns, the `PRAGMA table_xinfo` convergence check, the
  Postgres lag.
- Rustdoc, with `# Examples` on every new public item (AGENTS.md rule 10) and `# Errors` where fallible:
  `ServedArtifact`, `from_parts`, `EmbeddingFingerprint::served` and the nine-name parity list, `with_served`,
  `with_served_artifact`, `resolve_embedding`, `EmbeddingOverrides`, `ResolvedEmbedding`; plus `DEFAULT_QUERY_INSTRUCTION`, `migrate_v14_to_v15`, the new MCP/CLI
  keys.

## Testing

- **Unit:** fingerprint equality over every served field; serde key set, legacy default and flatten round-trip;
  `from_parts`; SQLite CRUD, partial-row rejection, both-direction mismatch, `begin_populating` resume; space names
  (legacy name kept for a plain identity);
  the reconstruct identity guard; `ensure_no_served`; `resolve_embedding` (every override, the query-instruction
  opt-out, partial and half configurations, the two dimension checks, the no-TOML `native_dim` fallback); CLI flags
  (including served flags without an embedder).
- **Migration:** v14 → v15 preserves the identity unserved; fresh vs migrated on `PRAGMA table_xinfo` plus the CHECK
  and index; idempotent on a current schema; insta and inline goldens reviewed.
- **Integration:** dump/restore of served spaces; first-write recording of a served artifact; reconstruction across
  served artifacts and resume of a legacy-named populating space; wiremock request bodies
  for the default instruction (queries prefixed, documents never, `""` opts out); the pre-computed guard; CLI `query`
  refusal.
- **Process:** `identity_startup.rs`: fifteen refusals (each sent `initialize` and never answered) and four serving
  controls (each answers `initialize`, then exits cleanly on EOF) against the real debug binary; CLI commands with an
  unreachable embedder fail cleanly instead of panicking.
- **Live Postgres (Docker, `#[ignore]`):** the unchanged v14 manifest, read as "Postgres unchanged".
- Excluded: no e2e against a real llama.cpp server (the sha is declared, not probed; dutiona/infra#77 verifies the
  file); no load tests.

## Verification

Every command in `AGENTS.md`'s Verify block, unpiped, at the final clean HEAD:

```bash
cargo build --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --all-features
cargo test  --workspace --all-features
cargo check -p memory-engine
cargo check -p memory-engine --no-default-features --features backend-sqlite
export RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links"
cargo doc --no-deps -p memory-engine
cargo doc --no-deps -p memory-engine --all-features
cargo deny check
cargo +nightly fuzz build        # the facade's EmbeddingFingerprint gains a field
```

Then: the live Postgres suite (item 5's verified command) with Docker; `git grep -n -- '--embed-dimensions' crates`
prints nothing; the golden diff of item 13 shows only the expected lines; `uv run sphinx-build -b html docs
docs/_build` adds no warning for the pages items 11 and 22 touch (the plan's own "not in any toctree" warning is
accepted: `/finish-pr` removes `docs/plans/` before merge). MSRV: not needed (no dependency or edition-sensitive change). CI is
not dispatched (no compute credit; the local gate is the merge gate).
