# Follow-up group A1: harden the embedding identity and the reconstruction backfill (#1126), successor v2

Successor of the plan approved on #1130 (SHA-256 `c264349119dc7facc565398b83cc7bd2306c43cef8899dfb5a43aeae1e456299`).
The delta, on Michaël's instruction of 2026-11-22: fold #1131 (found by sibling A2, #1127) as collateral (Decision 14,
item 15); record his confirmation of Decision 7; record the two follow-ups as filed (#1132, #1133); add A2's rebase
surface to the rebase item. Every other item is unchanged in intent; items 15–17 became 16–18.

## Outcome

What a store records of its embedding (an identity, a bare dimension, or nothing) is read read-only at any schema
version by one reader, `memory_engine::read_recorded_embedding`, whose SQL lives in `me-backend-sqlite` and is pinned
to equal what the migration chain produces. The one file-backed open seam, `MemoryEngine::open_from_config`, checks
the recorded dimension, and the identity a caller declares through `MemoryEngineBuilder<File>::declared_identity`,
before anything migrates. So `restore_sqlite` restores and migrates a v12, v13 or v14 backup (#1049), and a refused
server start leaves the store file byte-identical (#1065). A reconstruction completes past a fact without a vector
whose content the new provider rejects: that fact stays without a vector and in the queue, and `PromoteOutcome`
lists it (#1081). A `.pak` (format v3) records its vectors' identity; archive search scores vectors only for paks of
the active identity, still matches the others by text, and counts them (#1061). `add-fact --embedding` declares its
vector's identity with the same identity flags the embedder commands use, instead of a sentinel (#1060). The default
provider is one constant (#1064). Both binaries read the embedder key from a file (#1101). A test proves that
`erase`, `unforget`, `stats` and `export` open a store whose facts all wait, and that none of them records an
identity (#1097). The two binaries' copied dimension probes are deleted in favour of the reader. A JSON restore
restores each id sequence from the exported high-water mark, so an import after `erase` or archival reuses no id
(#1131, folded as collateral).

## Constraints

- #1126: "One PR that closes these issues, each in its own commit." / "Each member's own acceptance, unchanged. The PR carries one `Closes #N` line per member." / "Refs dutiona/infra#37"
- #1049: "Expected: the backup is restored and migrated to the current schema on open." / "Either way, correct the comment and add a test that restores a pre-v13 backup."
- #1060: "The CLI declares a pre-computed vector's identity exactly as the MCP tools do (served fields included) and refuses an undeclared vector when a store identity exists (ADR-0015 §3)." / "A failing test first; the stale TODO goes."
- #1061: "Each pak records the identity its vectors were made under." / "Archive search skips (or flags, with a reason) paks of another identity; a test covers a reconstructed store with an old pak."
- #1064: "One shared constant (e.g. in `memory-engine-embed`), referenced from both crates; no other literal remains."
- #1065: "The identity check runs read-only, before any migration; a refused start leaves the file byte-identical (test: checksum before and after)." / "The migration runs only once the identity check passes."
- #1081: "A failing test first: a backfill over a store with one permanently rejected item completes, embeds every other item, and reports the rejected one."
- #1097: "A store with facts but no recorded embedding identity opens for `erase`, `unforget`, `stats` and `export` (read-only commands without an identity; writing commands never stamp a guessed identity)." / "A test on a fresh store whose facts all wait for their vector covers each command."
- #1101: "`--embed-api-key-file` / `MEMORY_MCP_EMBED_API_KEY_FILE` on `memory-engine-mcp`, and the same on `memory-engine-cli` (`MEMORY_ENGINE_EMBED_API_KEY_FILE`). The file holds the key, with one trailing newline allowed." / "Setting the key itself and the key file at once refuses to start, naming both settings." / "A file that is missing, unreadable or empty refuses to start. The error names the path and never contains the file's contents." / "The key read from a file is held like today's key: redacted in `Debug`, never logged." / "The configuration reference documents the new setting next to `MEMORY_MCP_TOKEN_FILE`." / "Left out: permission checks on the file".
- #1131 (folded as collateral, Michaël 2026-11-22: "Get the follow-up from A2 folded into the plan and fixed."): "A failing test first: export, erase, restore, import; no id is reused and every call record still names its own fact." / "Sequences are restored from the exported high-water marks, not from the surviving rows." #1126's own scope is unchanged; the PR adds `Closes #1131`.
- Michaël at approval of #1130, on Decision 7: "I agree with the decision."
- ADR-0015 §2: "a store refuses a pre-computed first write whose declared identity differs from its configured provider's." §3: "a bare vector with no declared identity is rejected when a store identity exists, and a declaration that omits a field the store records does not match it."
- Settled, #1017 Decision 8: a pre-computed write "requires `declared == embedder.fingerprint()` before anything is written".
- Settled, #1021 Open question 7: "if the new provider rejects its content too, the reconstruction aborts as on any provider error (a limitation …)"; "A failed item holds the consolidation watermark until it is erased (#1023) or reconstructed."
- Owner: TDD, failing test first; a member already satisfied on main is proven by a test, not reimplemented. Each member and each boy-scout fix in its own commit; Conventional Commits, imperative, body says why, no Co-Authored-By on branch commits. Smallest complete change; no speculative options; delete before adding.
- Owner: merge gate = every AGENTS.md Verify command, unpiped, at the final clean HEAD; CI has no credit. Heavy commands under `flock /run/user/1000/memlane-heavy.lock` with `CARGO_BUILD_JOBS=16`. New SQLite tables or columns are listed on #634; a `.pak` format change bumps its version.
- Owner: change no function #1026 changes; rebase on main once #1026 merges, before the final gate. Public repository: no home paths, host names or tokens in commits, the PR or this plan. Merge stays with Michaël.

## Decisions

None is BLOCKING; each default is safe and reversible, and the PR body restates it.

1. **One reader, one decoder.** `me-backend-sqlite` `store::embedding_meta::load_recorded(&Connection) ->
   Result<RecordedEmbedding>` dispatches on `schema_version`: none or no `config` table → `Nothing`; above
   `CURRENT_SCHEMA_VERSION` → `Migration(SchemaVersionUnsupported)`; ≥ 15 → `find_active`; 13–14 → the same decoder
   with the four served columns selected as `NULL`; 12 → the `embedding_meta` value through `decode_v12_identity`,
   the decoder `migrate_v12_to_v13` itself now calls; ≤ 11 → no identity. With no identity, `STORE_DIM` gives
   `Dimension`. DTO `me_types::types::RecordedEmbedding { Identity(EmbeddingFingerprint), Dimension(usize), Nothing }`.
   Facade entry `memory_engine::read_recorded_embedding(&Path)`, beside the engine-free `read_*` family. Not on the
   storage port: it reads a file before any backend exists. #1049 is wider than its title: `find_active` selects the
   v15 columns, so v13 and v14 backups fail too ("no such column"), and item 4's tests show it.
2. **The checks live in the open seam, not in each caller.** `open_from_config`, for an existing file, reads
   `read_recorded_embedding` first: a recorded dimension (`RecordedEmbedding::dim()`: the identity's, or the bare
   `embed_dim` of a store whose facts all wait or of a pre-v12 store) other than `config.embed_dim` is today's
   `MigrationError::EmbedDimMismatch`; then a declared identity must match a recorded one (`EmbeddingModelMismatch`);
   both before the pool opens. A free check called before `build()` was rejected: it leaves the order to each caller,
   which is how #1065 happened. Every open refused for its dimension (an FTS-only server start, a CLI writer) then no
   longer migrates. New refusal: an all-waiting store opened at another dimension, which today opens and later mixes
   widths.
3. **`restore_sqlite` keeps its contract.** It refuses a copy with no `config.schema_version` (its own read-only
   query: the reader returns `Nothing` for a foreign SQLite file and for a blank store alike) as not a memory-engine
   database, as today (then by accident, via the missing table), so no ME schema is ever created inside a foreign
   file. It then checks the copy's recorded dimension through the reader, still returning `EmbeddingDimension` (as
   `restore_json` does), and opens through the seam.
4. **Byte identity** = the SHA-256 of the database file and of its `-wal` (absent read as empty) unchanged, and
   `schema_version` unchanged. `-shm` is excluded: SQLite's shared-memory index, which any reader rewrites; a read-only
   open may create an empty `-wal`/`-shm` beside a WAL store, as `status` already does.
5. **The probes fold in this PR** (item 6, its own droppable refactor commit): MCP `probe_embed_dim` and CLI
   `peek_embed_dim_from_db` become `read_recorded_embedding(..)?.dim()`; ~100 duplicated lines go and `rusqlite`
   moves to the server's `[dev-dependencies]` (#1026 adds no `rusqlite` use in `src/`).
6. **One home for the mismatch rule** (boy-scout, item 2): `EmbeddingFingerprint::ensure_matches(&self, candidate)`
   replaces the private `ensure_match` copies in both backends; the seam would otherwise add a third.
7. **#1081 skips only a fact that has no vector.** When a window's `embed_batch` answers `InputRejected` (400, 413,
   422), the window is embedded one input at a time. An input rejected alone whose fact has no vector (waiting or
   failed, `get_fact(..).embedding` empty) is skipped: nothing is written for it, it keeps `X''` and its queue row, and
   an in-memory set keeps both passes from re-sending it. The window cursor advances by the last id of the window as
   fetched, before skipped ids are dropped, and the loop ends only on an empty fetch, so a window made only of skipped
   ids never ends the pass early. An input rejected alone whose fact holds a vector, and any other error, aborts as
   today. Why: #1081's defect is "a deferred item that failed with 400/413/422 and that the new provider also
   rejects", and its acceptance's "permanently rejected item" is that item; a fact the old model embedded and the new
   one refuses is another case, and completing past it would drop that fact's only vector from the serving space, a
   policy change no issue asks for (reversible here: Michaël may choose "demote it to full-text and fail it in the
   queue" at approval; he confirmed the default: "I agree with the decision."). Consequences:
   no schema or port signature change, the cursorless resume holds (a rerun re-derives the same skips), no new error
   text is stored (a waiting fact the new provider rejects is failed later by the drain, through its scrubbed path; a
   failed one keeps its error), and a deferred fact stored after the catch-up pass no longer aborts the promote: it
   waits for the drain. `PromoteOutcome` gains `pub without_vector: Vec<i64>`.
8. **#1061.** `ArchivePak` gains `#[serde(default)] embedding_identity: Option<EmbeddingFingerprint>`;
   `CURRENT_PAK_VERSION` 2 → 3, so an older binary refuses a v3 pak rather than comparing its vectors blind (and, since
   one unreadable pak aborts its scan, loses archive search until upgraded; no shipped binary archives).
   `ARCHIVE_SCHEMA_VERSION` is unchanged (the `Fact`/`Edge` shapes are). A pak's vectors are scored iff both the pak
   and the store record an identity and the two are equal (`None` matches nothing, so v2 paks are text-only for good,
   and an unstamped store scores no pak); text matching is untouched, so archived facts stay findable after a
   reconstruction. The "flag, with a reason" is `QueryDiagnostics::archive_paks_unscored`, beside
   `archive_paks_scanned`. The identity a pak records is read in the same read transaction as its candidate facts and
   edges (`FactGraph::select_archive_candidates` returns it; only SQLite implements that port), so a promote can never
   fall between the two reads and stamp new-space vectors with the old identity. A promote that commits after that
   read and before `commit_archive_atomic` deletes the facts is accepted, not serialized: the pak still holds the
   vectors it captured under the identity it records, so nothing is ever scored across spaces; those facts merely
   become text-only one reconstruction early, the outcome of archiving them just before it, and their deletion is the
   archive's intended effect (the promote's retained rows cascade with them).
9. **#1060.** `IdentityArgs { embed_model: Option<String>, embed_provider: String, served: ServedArgs }` moves out of
   `EmbeddingArgs` unchanged (flags, env, the #1064 default) and is flattened by both `EmbeddingArgs` and
   `AddFactArgs`: a pre-computed vector is declared with exactly the identity the configured provider would report
   (#1017 Decision 8). `--embed-model` is required for `--embedding` on every store: a blank store has nothing to
   record but a guess. The open passes the declaration to `declared_identity`, so a mismatch is refused before
   anything migrates; `add_fact_precomputed` then records it on a blank store (ADR-0015 §2). No MRL flag: a store
   recorded under MRL refuses CLI vectors loudly.
10. **#1064.** `HttpEmbeddingProvider::DEFAULT_PROVIDER: &str = "ollama"`, beside `DEFAULT_QUERY_INSTRUCTION`.
    `KNOWN_PROVIDERS` keeps its own literal: it lists known backends, another concept, and deriving it from the default
    would drop `ollama` the day the default changes.
11. **#1101.** One loader, `memory_engine_embed::read_api_key_file(&Path) -> Result<String, MemoryError>`: reads the
    bytes, refuses non-UTF-8, strips one trailing `\n`, refuses an empty key and any control character left (so CRLF and
    a second line are refused); every error is `Conflict(PolicyParameter)` naming the path, never the contents.
    Exactly one key source: the file beside `--embed-api-key`/its env, or beside TOML `api_key`, is refused naming both.
    No TOML `api_key_file` (not asked). The server counts the file as an embedder setting (a lone one is refused, the
    #1017 rule). The CLI resolves the key settings in `build_optional` before it decides whether to build an embedder:
    the key and the file together, or a missing, unreadable or empty file, are refused even on an FTS-only `query`, and
    a key file without `--embed-url`/`--embed-model` is refused as the served flags are today. The key stays in the
    `Debug`-free types it lives in today.
12. **#1097 needs no code.** #1094 records `embed_dim` with a store's first waiting fact; item 1's test proves the four
    commands and that `erase`/`unforget` stamp nothing. A red assertion would turn it into the fix.
13. **Reported, not fixed; filed by the orchestrator:** (a) `batch-ingest`, `bootstrap` and `consolidate` could pass
    their provider's fingerprint to `declared_identity`; today they migrate, then refuse at the first write: #1132.
    (b) A writable open of a non-empty SQLite file with no `config` table creates ME's schema inside it: #1133.
14. **#1131: the export carries the id high-water marks.** A JSON export adds `sequences`, the `sqlite_sequence` rows
    (`name` → `seq`), read in the export's existing read transaction; `EngineSnapshot` gains
    `#[serde(default)] pub sequences: BTreeMap<String, i64>`. A restore sets each of the six tables it restores
    (`scopes`, `events`, `facts`, `edges`, `summaries`, `lineage`) to `max(sequences[table], highest restored id)`;
    other names in `sequences` are ignored (`activities` and `archive_manifest` are not restored). No range check:
    `max` already neutralizes a low or negative mark, and an absurd high one comes only from a hand-edited snapshot,
    which restore trusts as it trusts the row ids themselves (a restored fact id of `i64::MAX` breaks inserts the
    same way today). Compatibility, and its limit: a pre-#1131 export (no `sequences`)
    restores as today, from its rows, the only knowledge it has, so it can still reuse an id erased before it was
    taken; an older binary ignores the field (`EngineSnapshot` has no `deny_unknown_fields`) and restores the same
    way. Making an older reader refuse is not possible: its only gate is `schema_version`, the database schema, which
    this PR does not bump; bumping it for a snapshot field would take a schema migration and make older binaries
    refuse the store itself, a far larger break than the one it prevents. So the guarantee
    is stated as "an export taken by this version or later, imported by this version or later, reuses no id", and
    the docs say so. No snapshot version bump: the field is additive and defaulted, like `tombstones` and
    `lifecycle_receipts` before it. The SQLite
    export (`VACUUM INTO`) already copies `sqlite_sequence`, so it needs nothing. Not in this PR: whether A2's
    consolidation watermark may now be imported (#1127 excludes it from restore because ids were reused); its
    owner decides once this lands (rebase table, item 16).

**#1026 overlap.** The only function both branches change is `memory-engine-mcp` `main.rs` `run()`: this plan edits
steps 2 and 4–5, #1026 step 6 and adds `load_denylist()`; item 16 resolves it. Every other function named below is
outside #1026's footprint (`embed_queue::queue_or_reinforce`, `engine/backfill.rs`, `engine/lifecycle.rs`,
`bootstrap/`, `redact/`, `store/{facts,erase,lifecycle}.rs` are not touched; #1026 changes one test line of
`inspect/dump.rs`, not `stream_snapshot`). The functions this plan shares with A2's #1127 are listed in item 16.

## Implementation sequence

`h` is `h() { flock /run/user/1000/memlane-heavy.lock env CARGO_BUILD_JOBS=16 "$@"; }`; every command runs unpiped
from the worktree root. Each test is written first and seen failing with its red command. Items marked (mechanical)
may go to one local implementer at a time; the worker keeps design, guard and deletion logic, redaction, and every
verification. Each member's docs land in its own commit.

- [ ] **0. Worktree and plan.** No new worktree: `.worktrees/fix-1126-backfill-identity` on `fix/1126-backfill-identity`
  (from `origin/main` c103cfa) exists and is locked (`git worktree list --porcelain` shows `locked`). The plan issue
  is published once by `~/.claude/skills/finish-pr/scripts/archive-plan-issue.sh` before approval. After approval the
  plan and its review artifacts are committed with `git add -f` (this clone's `info/exclude` lists `/docs/plans/`);
  the drafts directory is deleted, never committed. Commit:
  `docs(plans): plan follow-up group A1 (#1126)`.

- [ ] **1. #1097: prove the maintenance commands open a store whose facts all wait.** (mechanical)
  File: `crates/memory-engine-cli/tests/cli_integration.rs`. Test
  `a_store_whose_facts_all_wait_serves_every_maintenance_command`: a dim-4 file store, three `add_fact_deferred`
  facts, the first forgotten with `forget_fact`, engine closed. Through the binary: `--format plain stats` prints
  `facts.active=2`; `export -o <tmp>/s.json` exits 0 and holds the waiting content; `unforget 1` exits 0 and `stats`
  then prints `facts.active=3`; `erase --fact 2` prints `facts=1`. After `unforget` and after `erase`, a read-only
  rusqlite connection finds no `embedding_spaces` row and `config.embed_dim = 4`. Expected green on main:
  `h cargo test -p memory-engine-cli --all-features --test cli_integration facts_all_wait` reports 3 passed (this and
  the two existing tests). A red assertion moves the fix into `crates/memory-engine-cli/src/db.rs` and the commit type
  to `fix(cli)`. Commit: `test(cli): prove the maintenance commands open a store whose facts all wait`.

- [ ] **2. Boy-scout: one home for the embedding-identity mismatch rule.**
  Files: `crates/me-types/src/types/cognitive.rs`, `crates/me-backend-sqlite/src/store/embedding_meta.rs`,
  `crates/me-backend-postgres/src/schema.rs`. Red: me-types `ensure_matches_is_field_by_field` (equal → `Ok`; each
  single differing field, served artifact included, → `EmbeddingModelMismatch { expected: self, actual: candidate }`);
  `h cargo test -p me-types --all-features ensure_matches` does not compile. Change:
  `impl EmbeddingFingerprint { pub fn ensure_matches(&self, candidate: &Self) -> Result<()> }` (`self` = the recorded
  identity); both private `ensure_match` go; SQLite `record_if_absent`/`check_compatible` and Postgres
  `record_embedding_fingerprint_if_absent`/`check_embedding_compatible` call it. Green: that command,
  `h cargo test -p me-backend-sqlite --all-features embedding_meta`; `git grep -nw ensure_match crates` prints nothing.
  Commit: `refactor(types): give the embedding-identity mismatch rule one home`.

- [ ] **3. The shared reader: what a store records of its embedding, at any schema version.**
  Files: `crates/me-types/src/types/cognitive.rs` (`RecordedEmbedding`),
  `crates/me-backend-sqlite/src/store/{embedding_meta.rs,embedding_spaces.rs}`,
  `crates/me-backend-sqlite/src/store/schema/{migrations.rs,mod.rs,tests.rs}`,
  `crates/memory-engine/src/{lib.rs,engine/mod.rs,engine/tests.rs}`, `tests/redaction_write_paths.rs`,
  `docs/design/schema-evolution-policy.md`.
  Red: `schema/tests.rs` `load_recorded_equals_the_migrated_identity` — fixtures v12 (`simulate_v12` + an MRL
  fingerprint as `embedding_meta` JSON), v13 (`simulate_v13` + an active row), v14 (`simulate_v14` + an active row),
  current (a served row), each also blank: `load_recorded` before `migrate` gives `Identity(fp)` (`Nothing` blank) and
  `embedding_meta::load` after `migrate` gives `Some(fp)` (`None`). `load_recorded_reads_a_dimension_or_nothing`: a
  store whose facts all wait → `Dimension(4)`; no `config` table → `Nothing`; v11 with `embed_dim` → `Dimension`;
  `CURRENT_SCHEMA_VERSION + 1` → `SchemaVersionUnsupported`; a corrupt v12 value → `Migration(Incompatible)` with the
  migration's text. Facade `read_recorded_embedding_never_writes`: a store rolled back to v14 by raw SQL returns its
  identity with the file's bytes unchanged; a missing path → `NotFound`.
  `h cargo test -p me-backend-sqlite --all-features load_recorded` and
  `h cargo test -p memory-engine --all-features recorded_embedding` do not compile.
  Change: `RecordedEmbedding` (`Debug, Clone, PartialEq, Eq`) with `dim() -> Option<usize>` and
  `identity() -> Option<&EmbeddingFingerprint>`; `pub(crate) fn decode_v12_identity(raw: &str) ->
  Result<EmbeddingFingerprint>` (the frozen `V12Fingerprint` decode, `served: None`), called by `migrate_v12_to_v13`;
  `find_active` and `find_active_before_v15` share a private `find_active_with(conn, cols)` over `row_to_space`;
  `load_recorded` per Decision 1; `pub fn read_recorded_embedding(db: &Path) -> Result<RecordedEmbedding>` in
  `engine/mod.rs` (`NotFound` unless `db.is_file()`, then `open_status_connection` and `load_recorded` in one read
  transaction; never validates or migrates), re-exported with `RecordedEmbedding`; EXEMPT gains
  `("read_recorded_embedding", "read")`. The policy doc says a migration that moves the identity must teach
  `load_recorded` its layout and add a fixture. Green: both commands; `h cargo test -p me-backend-sqlite --all-features
  store::` and `h cargo test -p memory-engine --all-features --test redaction_write_paths`.
  Commit: `feat(storage): read what a store records of its embedding at any schema version`.

- [ ] **4. #1049: restore a backup of any schema version.**
  Files: `crates/memory-engine/src/engine/{mod.rs,restore.rs,tests.rs}`, `tests/restore_roundtrip_test.rs`,
  `docs/usage/inspection.md`, `docs/reference/api.md`.
  Red: `restore_sqlite_migrates_an_older_backup` — a `dump_state(Sqlite)` backup of a store stamped `mock/test/4`,
  rolled back by raw SQL to v14 (served columns dropped), v13 (also `fact_vectors` dropped) and v12 (registry folded
  back into `config.embedding_meta`, table dropped): each restores, reads `CURRENT_SCHEMA_VERSION`, its fact and
  `EmbeddingFingerprint::new("mock","test",4)`. Today v12 fails `no such table: embedding_spaces`, v13/v14 `no such
  column: artifact_sha256`. `restore_sqlite_refuses_an_older_backup_of_another_dimension`: v12 and v14 backups into
  dim 8 → `EmbeddingDimension`, no target, `-wal` or `-shm` left; the same for a backup of a store whose facts all
  wait (dimension only). `engine/tests.rs` `an_open_at_another_dimension_never_migrates_an_older_store`: a v14 store
  at dim 4 opened by `builder(8)` → `EmbedDimMismatch`, bytes and `schema_version` 14 unchanged (Decision 4);
  `an_all_waiting_store_refuses_another_dimension`: a current store whose facts all wait (`embed_dim` 4) opened by
  `builder(8)` → `EmbedDimMismatch`, bytes unchanged (today it opens). The existing
  `restore_sqlite_probe_failure_leaves_no_sidecars` and `restore_sqlite_non_sqlite_file_returns_database_error` stay
  green (a foreign file gains no ME table). Red:
  `h cargo test -p memory-engine --all-features --test restore_roundtrip_test older_backup` and
  `h cargo test -p memory-engine --all-features -- never_migrates another_dimension` (two libtest filters after
  `--`) fail, each on its byte or version assertion.
  Change: `validate_embed_dim_against_meta(conn, dim)` becomes `validate_embed_dim(recorded: &RecordedEmbedding,
  dim) -> Result<()>` (`recorded.dim()` against `dim`, same error), called by `init_from_pool` on `load_recorded` of
  its connection and by `open_from_config` on `read_recorded_embedding(&config.path)?` when `config.path.is_file()`,
  before the pool. `restore_sqlite`: its `embedding_meta::load` probe goes; after the copy a read-only query refuses
  a copy with no `config.schema_version` (`Migration(Incompatible("… is not a memory-engine database"))`), then it
  compares `read_recorded_embedding(copy)?.dim()` (`EmbeddingDimension`), then `open_from_config`; every failure after
  the copy removes the target and its `-wal`/`-shm`. The false "Legacy v11 backups … would also skip the check" comment becomes: "A backup
  that recorded an identity, at any schema version, must agree on dimension; one with none has nothing to compare."
  Rustdoc and docs: "a backup of any schema version (v12 or later carries an identity)". Green: both commands;
  `h cargo test -p memory-engine --all-features --test restore_roundtrip_test` (the #1040 sidecar tests included).
  Commit: `fix(restore): restore a backup of any schema version`.

- [ ] **5. #1065: check the declared identity read-only, before the store migrates.**
  Files: `crates/memory-engine/src/engine/{builder.rs,mod.rs,tests.rs}`, `crates/memory-engine-mcp/src/main.rs`
  (`run()` steps 4–5), `crates/memory-engine-mcp/tests/identity_startup.rs`, `tests/redaction_write_paths.rs`,
  `docs/reference/{mcp-server.md,api.md}`.
  Red: `engine/tests.rs` `declared_identity_refusal_never_migrates_an_older_store` — a v14 store stamped `A`, in WAL
  and in rollback (`journal_mode=DELETE`) mode: `.declared_identity(B)` → `EmbeddingModelMismatch`, bytes and version
  unchanged; then `.declared_identity(A)` opens and the store reads `CURRENT_SCHEMA_VERSION`.
  `declared_identity_accepts_a_blank_store` (missing file, fresh store); a declared dim other than the engine's →
  `EmbeddingDimension`. `identity_startup.rs`: the shared `refuses()` helper also compares the store's bytes before
  and after, for every existing refusal; `a_refused_start_leaves_an_older_store_byte_identical` (a store stamped
  unserved, rolled back to v14, started under the served BASE config → `embedding identity check failed`; at
  `embed_dim = 512` with no embedder → `failed to open engine`; bytes and version unchanged; today it reads 18);
  `an_older_store_under_its_own_identity_serves_and_migrates`. `h cargo test -p memory-engine --all-features
  declared_identity` does not compile; `h cargo test -p memory-engine-mcp --all-features --test identity_startup
  older_store` fails on the identity case.
  Change: `MemoryEngineBuilder<File>::declared_identity(self, identity: EmbeddingFingerprint) -> Self` (file only;
  nothing is recorded at open, ADR-0015 §2); `EngineConfig` gains `pub(crate) declared_identity:
  Option<EmbeddingFingerprint>` (`None` in `new`, carried by `to_config`); `open_from_config`, in this order: a
  declared `dim` other than `config.embed_dim` → `EmbeddingDimension`; item 4's dimension rule
  (`EmbedDimMismatch`); then `recorded.identity()` `.ensure_matches(declared)`. So the existing `dim` case of
  `refuses_to_start_when_any_one_identity_field_differs` (engine and embedder at 512, store at 1024) still reads
  `failed to open engine`, now before any migration.
  `run()`: `builder.declared_identity(provider.fingerprint())` once the provider is built; the post-open
  `verify_embedding_identity` block goes; a build error maps `EmbeddingModelMismatch` → `embedding identity check
  failed: {e}`, anything else → `failed to open engine: {e}`; the lock is still taken before `build()`, so the read
  happens under it. EXEMPT gains `("declared_identity", "construction")`. `verify_embedding_identity` stays (CLI
  `query`'s read-only open). Docs: `mcp-server.md`'s "The refused start has already migrated the store to schema v15
  … rolling back is not one of the recoveries" becomes "A refused start leaves the store as it was: the check reads
  it read-only before anything migrates, so either binary still opens it"; `api.md` gains the builder row. Green: both
  commands; `h cargo test -p memory-engine-mcp --all-features --test identity_startup --test server_process` and
  `h cargo test -p memory-engine --all-features --test read_only_test`.
  Commit: `fix(embed): check the embedding identity read-only before migrating the store`.

- [ ] **6. Refactor: both binaries read the store's dimension through the reader.**
  Files: `crates/memory-engine-mcp/src/{config.rs,main.rs}` (`run()` step 2), `crates/memory-engine-mcp/Cargo.toml`,
  `crates/memory-engine-mcp/tests/config_merge.rs`, `crates/memory-engine-cli/src/db.rs`. Red: none (behaviour kept);
  new control `identity_startup.rs` `serves_an_older_store_without_a_configured_dimension` (v14 store, no `embed_dim`)
  passes before and after. Change: step 2 takes `embed_dim`, else `read_recorded_embedding(db)?.dim()`, else
  `cannot determine embed_dim: …`; `probe_embed_dim` and its tests go; `peek_embed_dim_from_db` becomes the same call
  with its "add a fact first" message; `rusqlite` moves to `[dev-dependencies]`. Green:
  `h cargo test -p memory-engine-mcp --all-features`, `h cargo test -p memory-engine-cli --all-features`;
  `git grep -nE 'embedding_spaces|embedding_meta' crates/memory-engine-mcp/src crates/memory-engine-cli/src` prints
  nothing. Commit: `refactor: read the store's dimension through read_recorded_embedding in both binaries`.

- [ ] **7. Boy-scout: `SchemaManager::promote_space`'s doc says same-dim only.** (mechanical) File:
  `crates/me-storage/src/schema.rs`: since #742 the promote is dimension-agnostic and "#623 T3" has landed; correct
  both. Check: `git grep -n 'Same-dim only' crates/me-storage` prints nothing. Commit:
  `docs(storage): promote_space is dimension-agnostic since #742`.

- [ ] **8. #1081: a reconstruction completes past a fact without a vector its provider rejects.**
  Files: `crates/memory-engine/src/engine/reconstruct.rs` (`backfill_space`, `reconstruct`),
  `crates/me-backend-sqlite/src/store/fact_vectors.rs` (`count_unbackfilled`, `promote_space`, module doc),
  `crates/me-types/src/types/cognitive.rs` (`PromoteOutcome`), `crates/me-storage/src/schema.rs` (docs of
  `count_unbackfilled`, `promote_space`), `docs/advanced/reconstruction.md`.
  Red: `reconstruct_leaves_a_fact_the_target_rejects_in_the_queue` (file engine: two facts embedded under the old
  identity, one deferred `poison` settled `Rejected` through `settle_embedding`, one deferred `poison too` still
  waiting; the target answers `InputRejected` for any batch or input holding `poison` and counts its calls):
  `reconstruct` is `Ok`, `without_vector == [poison, poison too]`, `promoted == 2`, the others hold the target's
  vectors, both poison facts keep `X''` and their queue rows (`failed == 1`, waiting 1), the identity is the target,
  and each poison input is sent at most twice (in its batch, then alone). `reconstruct_still_aborts_when_the_target_
  rejects_a_fact_with_a_vector` (`Err(Embedder(InputRejected))`, old identity, space still `populating`).
  `reconstruct_still_aborts_on_an_answer_that_is_not_a_rejection` (`NoAnswer`).
  `backfill_passes_a_window_made_only_of_skipped_facts` (`backfill_space` at batch size 2, `skipped` pre-filled with
  the ids of the four lowest facts: the two all-skipped windows do not end the pass, and the fifth fact is embedded).
  `reconstruct_resumes_past_facts_it_skipped` (the space opened and backfilled once by `backfill_space`, as a crash
  before the promote leaves it; then `reconstruct` completes with the same `without_vector`). `fact_vectors.rs`
  `promote_leaves_a_fact_without_a_populating_vector_in_the_queue` (keeps `X''` and its row; `without_vector ==
  [it]`); `promote_refuses_incomplete_populating` keeps guarding facts with a vector. `h cargo test -p memory-engine
  --all-features reconstruct_leaves` fails with `InputRejected`; `h cargo test -p me-backend-sqlite --all-features
  promote_leaves` does not compile.
  Change: `backfill_space(.., skipped: &mut BTreeSet<i64>)`: `after_id` takes the fetched window's last id, then ids
  in `skipped` are dropped from it (an emptied window fetches the next one; only an empty fetch ends the pass); on
  `Embedder(InputRejected)` the window is embedded one `embed` at a time in one `spawn_blocking`; an input rejected
  alone whose fact has no vector is added to `skipped`; one whose fact holds a vector, or any other error, returns
  (with a `tracing::warn!` naming the fact id). `reconstruct` shares one set across both passes. `count_unbackfilled`
  counts only facts `WHERE {HAS_VECTOR}` lacking a row in the space; `next_backfill_window` is unchanged.
  `promote_space`: the swap updates only `id IN (SELECT fact_id FROM fact_vectors WHERE space_id = ?1)`;
  `without_vector` is read before cleanup; the queue cleanup deletes only rows of facts now `{HAS_VECTOR}`.
  `PromoteOutcome::without_vector: Vec<i64>`; `promoted` = facts that took the new vector. Docs: the limitation of
  #1021 Open question 7 is lifted for facts without a vector. Green: both commands;
  `h cargo test -p memory-engine --all-features reconstruct`, `h cargo test -p me-backend-sqlite --all-features
  fact_vectors`. Commit: `fix(reconstruct)!: complete past a fact without a vector that the target rejects`
  (breaking: `PromoteOutcome` gains a public field).

- [ ] **9. Boy-scout: stale schema numbers in archive docs.** (mechanical) Files: `crates/me-archive/src/{pak.rs,
  manage.rs}`, `crates/me-types/src/types/archive.rs`: "`SQLite` is at 14" (it is at 18) loses the number and keeps
  the point; `build_pak`'s doc claims it is `pub` for a facade test (it is `pub(crate)`). Check:
  `git grep -n 'is at 14' crates/me-archive crates/me-types` prints nothing. Commit:
  `docs(archive): drop stale schema numbers and visibility notes`.

- [ ] **10. #1061: stamp the identity into `.pak` archives; score only same-identity vectors.**
  Files: `crates/me-archive/src/{types.rs,pak.rs,manage.rs,search.rs}`, `crates/me-types/src/types/search.rs`,
  `crates/me-storage/src/graph.rs`, `crates/me-backend-sqlite/src/sqlite/graph.rs`,
  `crates/memory-engine/src/engine/{archive.rs,query.rs}`, `tests/archive_test.rs`, `docs/advanced/reconstruction.md`.
  Red: me-archive `a_pak_records_its_identity` (round trip), `a_v2_pak_reads_without_an_identity`,
  `search_scores_vectors_only_within_one_identity` (paks of A, B and v2: a vector-only query under A returns A's fact,
  counts 2 unscored; under no active identity it returns nothing and counts 3; a text query returns all three);
  `read_pak_rejects_future_pak_version` tests 4; the serde-shape guard lists `embedding_identity`. Backend
  `select_archive_candidates_reads_the_identity_with_its_candidates` (returns the active identity, `None` on an
  unstamped store; extends `select_archive_candidates_parity_with_direct_store`). `tests/archive_test.rs`
  `an_old_pak_is_not_vector_scored_after_a_reconstruction`: archived under `mock/test/8`, a vector-only
  `include_archives` query finds fact 3 (`archive_paks_unscored == 0`); after `reconstruct` to `other/test/8` it does
  not (`== 1`), and a text query still does. `h cargo test -p me-archive --all-features identity` does not compile;
  `h cargo test -p memory-engine --all-features --test archive_test reconstruction` fails.
  Change: Decision 8's field and version. Port: `FactGraph::select_archive_candidates` returns
  `ArchiveCandidates { facts, edges, embedding_identity: Option<EmbeddingFingerprint> }` (a me-storage struct; the
  SQLite impl reads all three inside one `unchecked_transaction` on its read connection, the identity through
  `embedding_meta::load`). `build_pak(embed_dim, identity, facts, edges)`; `archive` stamps the candidates' identity.
  `search_archives(.., active: Option<&EmbeddingFingerprint>)` adds cosine only when `active.is_some() &&
  pak.embedding_identity.as_ref() == active`, and counts every other pak in `ArchiveSearchResult::paks_unscored` when
  the query has an embedding; `me_archive::manage::search_archives_fallback(cold, archive_dir, query, limit, active)`
  takes it from the facade's `MemoryEngine::search_archives_fallback`, which reads
  `self.storage.load_embedding_fingerprint()`; `execute_query` copies the count into `#[cfg(feature = "archive")]
  QueryDiagnostics::archive_paks_unscored`. Green: `h cargo test -p me-archive --all-features`,
  `h cargo test -p me-backend-sqlite --all-features select_archive_candidates`,
  `h cargo test -p memory-engine --all-features --test archive_test`, `h cargo check -p memory-engine`.
  Commit: `fix(archive)!: stamp the embedding identity into .pak archives and score only same-identity vectors`.

- [ ] **11. #1064: define the default embedding provider once.** (mechanical)
  Files: `crates/memory-engine-embed/src/http.rs`, `crates/memory-engine-mcp/src/config.rs`,
  `crates/memory-engine-mcp/tests/config_merge.rs`, `crates/memory-engine-cli/src/commands/{embedding_args.rs,
  batch_ingest.rs,bootstrap.rs}`. Red: `embed_provider_defaults_to_the_shared_provider` (clap default via
  `augment_args`) and `config_merge.rs` `the_section_and_the_resolver_default_to_the_shared_provider` do not compile.
  Change: Decision 10's constant; `default_provider()` returns it (serde needs a function; `resolve_embedding`'s
  fallback, the issue's "`build_embedder` fallback" since #1017, calls it); `default_value =
  HttpEmbeddingProvider::DEFAULT_PROVIDER`; the "Defaults to `ollama`" doc comments link the constant; the
  `EmbeddingArgs` test literals use it. The existing `"ollama"` assertions stay: they pin the value stores carry.
  Green: both tests; `git grep -n '"ollama"' crates/memory-engine-mcp/src crates/memory-engine-cli/src` shows only
  `KNOWN_PROVIDERS`. Commit: `refactor(embed): define the default embedding provider once`.

- [ ] **12. #1060: `add-fact` declares its vector's identity.**
  Files: `crates/memory-engine-cli/src/{commands/embedding_args.rs,commands/add_fact.rs,commands/batch_ingest.rs,
  commands/bootstrap.rs,db.rs}`, `crates/memory-engine-cli/tests/add_fact.rs`, `docs/reference/cli-inspector.md`.
  Red (every test clears `MEMORY_ENGINE_EMBED_*`): `add_fact_records_its_declared_identity_on_a_blank_store`
  (`--embed-model m --embed-provider p` → active row `m/p/4`, served NULL, never `precomputed`);
  `add_fact_records_a_declared_served_artifact`; `add_fact_refuses_an_undeclared_vector` (blank and stamped stores:
  exit 1 naming `--embed-model`, no fact, no new row); `add_fact_accepts_a_declaration_the_store_records`;
  `add_fact_refuses_a_declaration_the_store_does_not_record` (exit 1, `embedding model mismatch`, store bytes and
  version unchanged). Existing invocations gain `--embed-model test-model` (mechanical).
  `h cargo test -p memory-engine-cli --all-features --test add_fact declar` fails.
  Change: Decision 9's `IdentityArgs` (`EmbeddingArgs` flattens it as `identity`; `build_optional`/`build_inner` read
  `self.identity.*`); `IdentityArgs::declared(&self, dim) -> anyhow::Result<EmbeddingFingerprint>` (no model → "a
  pre-computed --embedding must declare its identity: --embed-model is required (ADR-0015 §3)");
  `db::open_engine_writable_with_dim(path, dim)` gains `declared: Option<EmbeddingFingerprint>` passed to
  `declared_identity`; `run` declares, opens, then `add_fact_precomputed(.., &declared, None)`. The sentinel and its
  `TODO(#615)` go. Green: `h cargo test -p memory-engine-cli --all-features`; `git grep -n 'TODO(#615)' crates` prints
  nothing. Commit: `fix(cli)!: declare add-fact's vector identity instead of a sentinel` (body: `--embed-model` is now
  required).

- [ ] **13. #1101: read the embedder API key from a file.**
  Files: `crates/memory-engine-embed/src/{key_file.rs (new),lib.rs}`, `crates/memory-engine-mcp/src/config.rs`
  (`EmbeddingOverrides`, `resolve_embedding`), `crates/memory-engine-mcp/tests/{config_merge.rs,server_process.rs}`,
  `crates/memory-engine-cli/src/commands/{embedding_args.rs,batch_ingest.rs,bootstrap.rs}`,
  `crates/memory-engine-cli/tests/cli_integration.rs`, `docs/reference/{mcp-server.md,cli-inspector.md}`.
  Red (every secret built at run time): `key_file` unit tests (`k` and `k\n` read `k`; missing path, directory, `""`,
  `"\n"`, `"k\nmore"`, `"k\r\n"`, non-UTF-8 refused; every error names the path and contains no file byte);
  `config_merge.rs` (the file supplies `api_key`; beside a flag/env key or a TOML `api_key` → refused naming both
  settings and no value; a lone file → refused by the url-and-model rule); `server_process.rs`
  `a_key_and_a_key_file_at_once_stop_startup_before_the_store_opens` (env only; exit 1; stderr names both variables,
  neither secret; no database file) and `a_key_file_never_reaches_the_log` (`RUST_LOG=trace`, starts and stops,
  stderr never holds the secret); CLI `embedding_args.rs` the same rules under its names, and `cli_integration.rs`
  `query_sends_the_key_from_a_key_file` (wiremock requires `Authorization: Bearer <key>`) and
  `an_fts_only_query_refuses_a_bad_key_setting` (no `--embed-url`: a missing key file, an empty one, and a key beside
  a key file each exit 1 naming the setting or path and no secret).
  `h cargo test -p memory-engine-embed --lib key_file` does not compile.
  Change: Decision 11's `read_api_key_file`, re-exported at the crate root, `# Errors` and `# Examples`; MCP
  `#[arg(long = "embed-api-key-file", env = "MEMORY_MCP_EMBED_API_KEY_FILE")] api_key_file: Option<PathBuf>`, resolved
  first in `resolve_embedding` (refusal: "the embedder key is set twice: --embed-api-key-file /
  MEMORY_MCP_EMBED_API_KEY_FILE and <--embed-api-key / MEMORY_MCP_EMBED_API_KEY | api_key in [embedding]>; set one");
  CLI `embed_api_key_file` (`--embed-api-key-file`, `MEMORY_ENGINE_EMBED_API_KEY_FILE`) resolved at the top of
  `build_optional`, before the url-and-model match (Decision 11), with the same rule under its names; `build_inner`
  takes the resolved key; test literals gain the field (mechanical). Docs: a row right after `--token-file` in
  `mcp-server.md` (one source, file format, mounted read-only like the token file); the shared embedder-flags sentence
  and `batch-ingest` table in `cli-inspector.md`. Green: `h cargo test -p memory-engine-embed --all-features`,
  `h cargo test -p memory-engine-mcp --all-features --test config_merge --test server_process`,
  `h cargo test -p memory-engine-cli --all-features`. Commit: `feat(config): read the embedder API key from a file`.

- [ ] **14. Boy-scout: `api.md` lists constructors that no longer exist.** (mechanical) File:
  `docs/reference/api.md`: the Lifecycle rows `open(config)` and `open_memory(embed_dim)` (replaced by
  `MemoryEngine::builder` in #541) become one `builder` row. Check: the Sphinx command passes. Commit:
  `docs(api): list the builder instead of the removed open constructors`.

- [ ] **15. #1131 (collateral): restore id sequences from the exported high-water marks.**
  Files: `crates/me-types/src/types/inspect.rs` (`EngineSnapshot`), `crates/me-backend-sqlite/src/inspect/dump.rs`
  (`stream_snapshot`, its schema and golden tests), `crates/memory-engine/src/inspect/restore.rs`
  (`restore_snapshot_into` step 9; its eleven test `EngineSnapshot` literals gain `sequences`),
  `crates/memory-engine/src/inspect/types.rs` (the `engine_snapshot_roundtrip` literal),
  `crates/memory-engine/src/engine/inspect.rs` and its `snapshot_populated_engine_dump` golden under
  `crates/memory-engine/src/engine/snapshots/`, `tests/restore_roundtrip_test.rs`, `docs/reference/cli-inspector.md`.
  The acceptance's "export, erase, restore, import" maps to: erase (which makes the gap), export, restore the export
  into a new store (the CLI's `import`), then write new memory into it; exporting before the erase would carry the
  erased fact and test nothing.
  Red: `an_import_after_erase_reuses_no_id` — a file store with facts 1–3, facts 2 and 3 each written as the MCP's
  `remember` writes one: a call-record event first, then the fact with `source_event_id` pointing at it; `erase` of
  fact 3, whose erase record names `{fact: 3, source_event: <its record>}` in `linked` (`store/erase.rs`, the #1020
  link); `dump_state(Json)`; `restore_json` into a new path; then one more call record and its fact. Asserts: the
  new fact's id is 4 (today 3); `get_fact(3)` is `None`, so the erase record's `linked` names no live fact and no
  live fact points at fact 3's record; fact 2 still points at its own record. `restore_takes_each_exported_mark_or_the_highest_row` (`inspect/restore.rs` unit test):
  a snapshot whose `sequences` set each of the six tables 10 above its highest row restores those six marks into
  `sqlite_sequence`; one whose marks sit below the highest rows restores the rows' maxima; `activities` in
  `sequences` is ignored. `a_snapshot_without_sequences_restores_from_its_rows`: the same export with `sequences`
  removed restores and a new fact takes the id after the highest row (today's behaviour, pinned).
  `an_export_with_sequences_reads_as_a_snapshot_with_an_unknown_field`: the snapshot JSON with an extra unknown key
  deserializes, the property an older binary's reader relies on to ignore `sequences`. `h cargo test -p memory-engine --all-features --test restore_roundtrip_test
  reuses_no_id` fails (id 3); `h cargo test -p memory-engine --all-features exported_mark` does not compile.
  Change: Decision 14. `stream_snapshot` writes `sequences` after `config`, keeping the three leading scalars first
  (the `peek_embed_dim_from_reader` invariant); `restore_snapshot_into` step 9 passes `max(seq, max id)` to the
  unchanged `reset_autoincrement`. Goldens: `snapshot_empty_engine_dump`'s snapshot gains `sequences` with the
  `scopes` mark the root scope sets, and `snapshot_populated_engine_dump`'s gains its marks (whatever the real output
  is, each reviewed before it is accepted); `streaming_output_matches_snapshot_schema` covers the new key. Docs: the `export`/`import` section of
  `cli-inspector.md` says a JSON export keeps the id high-water marks, so an export taken by this version or later,
  imported by this version or later, never reuses an erased or archived id; an older export or an older binary
  restores from the surviving rows, as before. Green: that command, `h cargo test -p memory-engine --all-features --test restore_roundtrip_test`,
  `h cargo test -p memory-engine --all-features inspect::`, `h cargo test -p memory-engine --all-features
  engine::inspect` (the populated golden, reviewed like the empty one), `h cargo test -p me-backend-sqlite
  --all-features inspect::dump`. Rustdoc: `EngineSnapshot::sequences` says what it holds and that it is absent, so
  empty, in a pre-#1131 snapshot. Commit: `fix(restore): restore id sequences from the exported high-water marks` (body: found by
  #1127's planning; `Closes #1131` goes in the PR body).

- [ ] **16. Rebase on #1026 (and #1127 if it merged first).** `git fetch origin && git rebase origin/main`. Keep
  #1026's step 6 and `load_denylist()` with this branch's `run()` (lock → `declared_identity` build → no post-open
  check; step 2 through the reader); keep both sides of `lib.rs` re-exports, `engine/mod.rs`'s module list, the
  GATED/EXEMPT lists and `mcp-server.md`; keep `rusqlite` a normal dependency if #1026 now uses it in `src/`. If either
  branch bumped the schema, re-check `load_recorded`'s dispatch and add its fixture. Rerun Verification in full; commit
  only if a resolution changed code (`fix: reconcile the identity check with #1026`).
  A2 (#1127, plan #1129) shares these functions; whichever lands second keeps both changes:

  | Function or item | This plan | #1127 |
  |---|---|---|
  | `store::fact_vectors::promote_space` | item 8: swap and queue cleanup limited to facts with a populating vector; `without_vector` | rewinds the dedup watermark and bumps the generation in the same transaction |
  | `SchemaManager::promote_space` docs (`me-storage/src/schema.rs`) | items 7, 8 | contract docs |
  | `inspect::restore` | item 15: `restore_snapshot_into` step 9, `validate_snapshot` | `MANAGED_CONFIG_KEYS` gains the watermark and generation keys (one line); with item 15 ids are no longer reused, which #1127's reason for excluding them assumed: flagged to its owner, not changed here |
  | `me-types/src/types/cognitive.rs` | `RecordedEmbedding`, `EmbeddingFingerprint::ensure_matches`, `PromoteOutcome::without_vector` | `PromoteRequest::embedding_fingerprint` |
  | `me-storage/src/graph.rs`, `sqlite/graph.rs` | item 10: `select_archive_candidates` returns `ArchiveCandidates` | the `types::pruning::PruneStats` rename (its item 9, the `me-forget` → `me-prune` crate rename, rebases last) |
  | `memory-engine/src/lib.rs`, `engine/tests.rs` | re-exports and tests added | re-exports renamed, tests added |

- [ ] **17. PR.** `gh pr create --base main --head fix/1126-backfill-identity --body-file <file outside the tree>`,
  titled `fix!: harden the embedding identity and the reconstruction backfill (follow-up group A1)`. Body: route lines
  (`Classification: Bug fix (user-confirmed)`, `Execution path: Standard`, `Planning tier: Super`,
  `Post-implementation review: Full (Default severity)`); one line each `Closes #1126`, `Closes #1049`, `Closes #1060`,
  `Closes #1061`, `Closes #1064`, `Closes #1065`, `Closes #1081`, `Closes #1097`, `Closes #1101`, `Closes #1131`; `Refs
  dutiona/infra#37` (no closing keyword beside it); the plan issue; the acceptance map (Testing), whose #1131 row
  carries item 15's mapping of "export, erase, restore, import" to the test's order; the breaking changes
  for the squash message (`add-fact` requires `--embed-model`; `.pak` v3, which older binaries refuse;
  `PromoteOutcome::without_vector`; `QueryDiagnostics::archive_paks_unscored`; every file-backed open checks the
  recorded dimension before migrating, and a store whose facts all wait refuses another dimension; a refused server
  start leaves the store as it was; the `FactGraph::select_archive_candidates` port returns `ArchiveCandidates`;
  `EngineSnapshot` gains `sequences`; the
  CLI refuses a bad key setting even on an FTS-only `query`); the Decisions; "no new SQLite table or column: nothing
  to list on #634";
  `Collateral findings` (#1131 folded, item 15; #1132 and #1133 filed, Decision 13); the gate output.

- [ ] **18. `/super-review` (Full gate, persisted route), then `/finish-pr` to READY.** Each finding is fixed in its own
  commit, test first, or answered with evidence; Verification reruns after the last fix; `/finish-pr` archives the
  plan on its plan issue and removes `docs/plans/`. Merge: Michaël, through `/squash-merge`; the worker never merges.

## Documentation

- `docs/reference/mcp-server.md`: the read-only identity check before any migration and the corrected recovery
  sentence (item 5); `embed_dim` probed "at any schema version" (item 6); the `--embed-api-key-file` /
  `MEMORY_MCP_EMBED_API_KEY_FILE` row after `--token-file`, and the TOML `api_key` comment pointing at it (item 13).
- `docs/reference/cli-inspector.md`: a new `add-fact` section (identity flags, `--embed-model` required, what a blank
  store records, refusal before the writable open, MRL stores refuse) (item 12); `--embed-api-key-file` (item 13); the JSON
  export keeps the id high-water marks and its compatibility limit (item 15).
- `docs/advanced/reconstruction.md`: backfill skips a rejected fact without a vector, aborts on one with a vector;
  the promote leaves such facts in the queue and lists them in `without_vector` (item 8); a pak keeps its identity
  and only same-identity paks are vector-scored, counted in `archive_paks_unscored` (item 10).
- `docs/usage/inspection.md`, `docs/reference/api.md`: `restore_sqlite` takes a backup of any schema version (item 4);
  `read_recorded_embedding` and `declared_identity` rows (items 3, 5); the builder row (item 14).
- `docs/design/schema-evolution-policy.md`: the reader's per-layout obligation (item 3).
- Rustdoc with `# Errors` and `# Examples` on every new public item (`RecordedEmbedding`, `ensure_matches`,
  `load_recorded`, `read_recorded_embedding`, `declared_identity`, `read_api_key_file`, `DEFAULT_PROVIDER`), and the
  changed contracts of `restore_sqlite`, `open_from_config`, `reconstruct`, `count_unbackfilled`, `promote_space`,
  `PromoteOutcome`, `ArchivePak::embedding_identity`, `CURRENT_PAK_VERSION`, `QueryDiagnostics::archive_paks_unscored`,
  `resolve_embedding`, `EngineSnapshot::sequences` and `restore_snapshot_into`.

## Testing

Unit tests sit where each rule lives (the reader beside the migration fixtures, the promote gate in `fact_vectors`,
the scoring rule in me-archive, the key file in the embed crate); facade integration tests prove each acceptance on
real files; process tests run the real server binary (`identity_startup`, `server_process`). Old schemas come from
`simulate_v12/13/14` in the backend and raw-SQL rollbacks of a current store elsewhere. Items 1, 6 and 11 have no red
and say why (fixed on main, behaviour-kept refactor, value-kept constant).

| Member | Acceptance | Tests | Item |
|---|---|---|---|
| #1049 | a pre-v13 backup restores and migrates; comment corrected | `restore_sqlite_migrates_an_older_backup` (v12–v14), `restore_sqlite_refuses_an_older_backup_of_another_dimension`, `load_recorded_equals_the_migrated_identity` | 3, 4 |
| #1060 | declared identity, served included; undeclared refused; test first; TODO gone | the five `add_fact_*` tests; grep | 12 |
| #1061 | pak records its identity; other identities not vector-scored, flagged; reconstructed store with old pak | `a_pak_records_its_identity`, `search_scores_vectors_only_within_one_identity`, `an_old_pak_is_not_vector_scored_after_a_reconstruction` | 10 |
| #1064 | one constant, both crates, no other literal | the two default tests; grep | 11 |
| #1065 | read-only before migration; byte-identical; migration only after a pass | `declared_identity_refusal_never_migrates_an_older_store`, `a_refused_start_leaves_an_older_store_byte_identical`, the byte check in `refuses()`, `an_older_store_under_its_own_identity_serves_and_migrates` | 4, 5 |
| #1081 | completes, embeds every other item, reports the rejected one | `reconstruct_leaves_a_fact_the_target_rejects_in_the_queue` and its two abort siblings, `promote_leaves_a_fact_without_a_populating_vector_in_the_queue` | 8 |
| #1097 | four commands open; writers stamp nothing | `a_store_whose_facts_all_wait_serves_every_maintenance_command` | 1 |
| #1101 | flag/env on both binaries; one source; missing/unreadable/empty refused without contents; never logged | `key_file` tests, `config_merge`, `server_process`, CLI tests | 13 |
| #1131 (collateral) | export, erase, restore, import: no id reused, call records name their own facts; sequences from the exported marks | `an_import_after_erase_reuses_no_id`, `restore_takes_each_exported_mark_or_the_highest_row`, `a_snapshot_without_sequences_restores_from_its_rows`, `an_export_with_sequences_reads_as_a_snapshot_with_an_unknown_field` | 15 |

Regression suites stay green and change only where an item says so: `crates/memory-engine-mcp/tests/*`,
`crates/memory-engine-cli/tests/*`, `tests/{restore_roundtrip_test,archive_test,redaction_write_paths,read_only_test}.rs`,
the reconstruction, `fact_vectors`, `embed_queue`, schema-migration and `.pak` proptest suites. Excluded: a live
embedder (all are scripted or wiremock), a Postgres data path (its reconstruction is stubbed; the ignored suite runs
because item 2 edits its backend).

## Verification

At the final clean HEAD after item 16, on the host, unpiped, each to completion; CI stands in for none of it. Each
command exits 0 and every test summary reports 0 failed.

```bash
h() { flock /run/user/1000/memlane-heavy.lock env CARGO_BUILD_JOBS=16 "$@"; }
h cargo build --workspace --locked
cargo fmt --all --check
h cargo clippy --workspace --all-targets --all-features -- -D warnings
h cargo build --workspace --all-features
h cargo test  --workspace --all-features
h cargo check -p memory-engine
h cargo check -p memory-engine --no-default-features --features backend-sqlite
export RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links"
h cargo doc --no-deps -p memory-engine
h cargo doc --no-deps -p memory-engine --all-features
h cargo deny check
uv run --locked sphinx-build -b html -W --keep-going -E docs docs/_build
h cargo +nightly fuzz build                                          # the facade API changes
h cargo +1.89 build --workspace --tests --examples                   # MSRV: a manifest changes (item 6)
h cargo +1.89 build --workspace --tests --examples --all-features
h cargo test -p me-backend-postgres --features test-util -- --ignored --test-threads=3   # item 2 edits it; container
```

Then each prints nothing: `git grep -nw ensure_match crates`, `git grep -n 'TODO(#615)' crates`,
`git grep -nE 'embedding_spaces|embedding_meta' crates/memory-engine-mcp/src crates/memory-engine-cli/src`,
`git grep -n 'Same-dim only' crates/me-storage`. Image build: N/A unless `Cargo.lock` changes (item 6 moves a
dependency's kind only; `--locked` proves it). Plugin gate: N/A (`plugins/memory-engine/` untouched).
