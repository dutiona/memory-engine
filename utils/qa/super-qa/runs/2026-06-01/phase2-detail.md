# Super QA — Phase 2 Consolidated Detail (2026-06-01-233426)

Generated from 85-agent deep-dive (workflow wf_43a240d4-be5) + Phase-1 + supply-chain. Severities recalibrated against primary source where marked **[V]** (verified). Security findings ran via `octo:personas:security-auditor` (tier-2 fallback, xhigh not effort:max) — marked **[FB]**.

## Severity summary

| Severity | Count | Auto-fixable | Report-only |
| --- | ---: | ---: | ---: |
| blocker | 1 | 1 | 0 |
| critical | 2 | 1 | 1 |
| high | 82 | 5 | 77 |
| medium | 223 | 57 | 166 |
| low | 236 | 114 | 122 |
| info | 38 | 18 | 20 |
| **Total** | **582** | **196** | **386** |

## Verified findings (primary-source / compiler-confirmed)

| Sev | ID | Location | Title | Verification |
| --- | --- | --- | --- | --- |
| blocker | `engine/build-archivedup` | src/engine/mod.rs:48-52 | Duplicate mod archive breaks --all-features | VERIFIED compiler E0428/E0592 |
| critical | `engine/build-notifyinsert` | src/engine/cognitive.rs:224 | notify_insert missing on HnswStrategy (ann) | VERIFIED compiler E0599 |
| critical | `cli/correctness-batch-ingest-read-only` | memory-engine-cli/src/commands/batch_ingest.rs | batch-ingest opens existing DB read-only then attempts writes —  | VERIFIED: else-branch open_engine(read_only=true) then add_facts_batch writes; o |
| high | `graph/correctness-remove-node-stale-nodemap` | src/graph/memory_graph.rs:82-87 | remove_node leaves node_map with a stale NodeIndex for the displ | VERIFIED real by source; DORMANT — only caller engine/archive.rs:86 is #[cfg(fea |
| high | `store/design-schema-ddl-divergence` | src/store/schema.rs:686-687 (migrate_v8_to_v9) | idx_activities_dedup index column count diverges between fresh i | VERIFIED: migrate line687=5cols(+scope_id) vs TABLES_DDL line819=4cols |
| high | `engine/msrv-violation` | src/engine/activity_filter.rs:99 | API stable-since-1.91 vs MSRV 1.85 | VERIFIED clippy incompatible_msrv |
| high | `workspace/clippy-gate-broken` | workspace | clippy -D warnings gate fails (default) | VERIFIED exit101 |
| high | `workspace/test-allfeatures-broken` | workspace | cargo test --all-features broken via #1 | VERIFIED |
| high | `search/soundness-hnsw-panic-corrupts-index` | src/search/ann.rs:333-341 | assert! in notify_insert panics with permanently-corrupt HNSW in | VERIFIED: index.insert mutates before assert_eq; parking_lot no-poison; ann-gate |
| high | `scope/soundness-no-cycle-guard` | src/scope/tree.rs:61-68, 147-168 | ancestors and path_for_id loop forever on cyclic parent links | VERIFIED: no visited-set guard; add cycle/depth guard regardless of reachability |
| high | `store/explicit-counter-loop` | src/store/scopes.rs:116 | explicit_counter_loop denied error | VERIFIED clippy error |
| medium | `store/dead-code` | src/store/activities.rs:101;checkpoints.rs:47 | never-used methods | VERIFIED clippy dead_code |
| medium | `pool/soundness-write-bypasses-readonly-guard` | src/pool/connection_pool.rs:206-208 | pub write() bypasses the read-only guard enforced by try_write() | VERIFIED: query_only=ON backstops write_conn in RO pool + callers use writable p |
| medium | `workspace/clippy-warnings` | workspace | 226 clippy warnings backlog | VERIFIED |
| medium | `sc/no-advisory-gate` | workspace | no cargo-audit/deny/deny.toml advisory gate | main-loop |
| low | `docs/crate-count-drift` | CLAUDE.md | says 3 crates, actual 4 | VERIFIED |
| info | `sc/no-release-profile` | Cargo.toml | no [profile.release] tuning | main-loop |
| info | `sc/dup-deps` | Cargo.lock | 11 duplicate transitive dep versions | main-loop |

## BLOCKER (1)

| ID | Cat | Location | Title | Auto | Flags |
| --- | --- | --- | --- | :-: | --- |
| `engine/build-archivedup` | build | src/engine/mod.rs:48-52 | Duplicate mod archive breaks --all-features | Y | [V] |

## CRITICAL (2)

| ID | Cat | Location | Title | Auto | Flags |
| --- | --- | --- | --- | :-: | --- |
| `engine/build-notifyinsert` | build | src/engine/cognitive.rs:224 | notify_insert missing on HnswStrategy (ann) | N | [V] |
| `cli/correctness-batch-ingest-read-only` | correctness | memory-engine-cli/src/commands/batch_ingest. | batch-ingest opens existing DB read-only then attempts writes — always | Y | [V] |

## HIGH (82)

| ID | Cat | Location | Title | Auto | Flags |
| --- | --- | --- | --- | :-: | --- |
| `graph/correctness-remove-node-stale-node` | correctness | src/graph/memory_graph.rs:82-87 | remove_node leaves node_map with a stale NodeIndex for the displaced n | N | [V] |
| `archive/correctness-tmp-file-leak` | correctness | src/archive/pak.rs:38-43 | Orphan .pak.tmp file on serialization or encoder-flush failure | N |  |
| `archive/correctness-orphan-pak-on-commit` | correctness | src/engine/archive.rs:65-79 | Orphan .pak file on commit_archive failure leaves unregistered pak on  | N |  |
| `consolidation/correctness-stale-importan` | correctness | src/consolidation/dedup.rs:93-103 | Stale in-memory importance_score causes incorrect inheritance in multi | N |  |
| `embed/correctness-batch-missing-direct-f` | correctness | memory-engine-embed/src/http.rs:216-230 | embed_batch() does not handle the direct `embedding` response format t | Y |  |
| `embed/correctness-utf8-panic-truncation` | correctness | memory-engine-embed/src/http.rs:222-224 | Byte-indexed String slice `&body_str[..1000]` panics on multi-byte UTF | Y |  |
| `engine/correctness-orphan-pak` | correctness | src/engine/archive.rs:65-80 | Orphaned .pak file on commit_archive failure | N |  |
| `engine/correctness-silent-promote-err` | correctness | src/engine/activity.rs:131-134 | Promotion failure silently swallowed, caller sees Recorded status | Y |  |
| `mcp/correctness-flush-insights-no-cap` | correctness | memory-engine-mcp/src/tools/mod.rs:840-843 | flush_insights accepts unbounded insights array from untrusted MCP cal | N |  |
| `mcp/correctness-bootstrap-no-size-cap` | correctness | memory-engine-mcp/src/tools/mod.rs:1225-1239 | bootstrap_session accepts unbounded jsonl_data string from untrusted M | N |  |
| `store/design-schema-ddl-divergence` | design | src/store/schema.rs:686-687 (migrate_v8_to_v | idx_activities_dedup index column count diverges between fresh install | N | [V] |
| `archive/design-archivepak-no-version-che` | design | src/archive/pak.rs:58-67, src/archive/types. | `ArchivePak` has no constructor and `read_pak` does not validate `pak_ | N |  |
| `cli/design-stringly-typed-fact-type-trip` | design | memory-engine-cli/src/commands/add_fact.rs:1 | Three parallel fact-type converters mapping `FactType` variants: two e | N |  |
| `cli/maintainability-near-zero-test-cover` | design | memory-engine-cli/src/ | 11 of 12 source files lack any inline tests — critical paths entirely  | N |  |
| `consolidation/design-usize-max-sentinel` | design | src/consolidation/dedup.rs:43-45, src/consol | Magic usize::MAX sentinel encodes 'skipped' status in numeric return v | N |  |
| `consolidation/design-pseudo-facts-leaky-` | design | src/consolidation/global.rs:34-57 | global_integration constructs fake Fact structs from Summary to satisf | N |  |
| `core-root/design-glob-reexport-error` | design | src/lib.rs:52 | Glob re-export of error module creates implicit public API surface | N |  |
| `core-root/design-glob-reexport-types` | design | src/lib.rs:58 | Glob re-export of types module exposes ~30 types without an explicit c | N |  |
| `core-root/design-importance-field-ambigu` | design | src/types.rs:133-139 | `Fact` has two importance fields with near-identical names but differe | N |  |
| `embed/design-duplicated-http-plumbing` | design | memory-engine-embed/src/http.rs:116-181,183- | HTTP request/response plumbing duplicated verbatim between `embed` and | N |  |
| `inspect/design-impl-submodules-overcalib` | design | src/inspect/mod.rs:6-11 | Implementation submodules exposed as `pub` despite no external callers | N |  |
| `mcp/design-god-module-tools` | design | /home/mroynard/dev/memory-engine/memory-engi | God module: 1386-line flat file mixing tool schema, dispatch, and all  | N |  |
| `pool/design-inmemory-reentrant-deadlock` | design | src/pool/connection_pool.rs:187-192 | read() in in-memory mode acquires write_conn.lock(), creating a deadlo | N |  |
| `resume/design-impure-default-now` | design | src/resume/context.rs:29-41 | ResumeConfig::Default captures wall-clock time — impure and stale-pron | N |  |
| `store/design-god-module-schema` | design | src/store/schema.rs (2491 lines) | schema.rs is a 2491-line god module mixing DDL, migrations, config, ba | N |  |
| `store/design-stringly-typed-relation-typ` | design | src/store/edges.rs:21,49 (and types.rs NewEd | relation_type is a bare String throughout EdgeStore — no newtype or en | N |  |
| `core-root/documentation-fact-dual-import` | documentation | src/types.rs:133,139 | Fact struct: importance and importance_score are both undocumented wit | N |  |
| `embed/documentation-embed-no-doc` | documentation | /home/mroynard/dev/memory-engine/memory-engi | `EmbeddingProvider::embed` impl has no /// doc comment | N |  |
| `embed/documentation-embed-batch-no-doc` | documentation | /home/mroynard/dev/memory-engine/memory-engi | `EmbeddingProvider::embed_batch` impl has no /// doc comment | N |  |
| `graph/documentation-remove-node-stale-in` | documentation | src/graph/memory_graph.rs:78-87 | remove_node doc omits NodeIndex invalidation side-effect; silent corre | N |  |
| `search/documentation-results-returned-ne` | documentation | src/search/hybrid.rs:77,247-252 | `QueryDiagnostics::results_returned` documented but never assigned | Y |  |
| `search/documentation-notify-insert-panic` | documentation | src/search/ann.rs:324-344 | `HnswStrategy::notify_insert` panics in production code with no `# Pan | N |  |
| `archive/performance-full-table-scan-cand` | performance | src/engine/archive.rs:153-161 (calls src/sto | select_archive_candidates loads entire facts and edges tables into mem | N |  |
| `search/perf-n1-hybrid-fetch` | performance | src/search/hybrid.rs:207-214 | N+1 SQLite round-trips in hybrid_search result collection | N |  |
| `search/perf-n1-hnsw-filter` | performance | src/search/ann.rs:279-287 | 2N SQLite round-trips per HNSW candidate in HnswStrategy::search | N |  |
| `engine/msrv-violation` | portability | src/engine/activity_filter.rs:99 | API stable-since-1.91 vs MSRV 1.85 | N | [V] |
| `workspace/clippy-gate-broken` | process | workspace | clippy -D warnings gate fails (default) | N | [V] |
| `workspace/test-allfeatures-broken` | process | workspace | cargo test --all-features broken via #1 | N | [V] |
| `archive/path-traversal-broken-startswith` | security | src/archive/search.rs:44-54 | Path-traversal guard is ineffective: lexical Path::starts_with does no | N | [FB] |
| `bootstrap/dos-unbounded-parse-allocation` | security | src/bootstrap/parse.rs:91-116 | Unbounded memory allocation parsing untrusted JSONL session logs | N | [FB] |
| `inspect/dos-decompression-bomb` | security | src/inspect/restore.rs:71-90 (read_snapshot) | Decompression bomb: size cap applies to compressed file, not decompres | N | [FB] |
| `mcp/dos-unbounded-embedding-vec` | security | memory-engine-mcp/src/tools/mod.rs:467-474 ( | Unbounded Vec<f32> allocation from untrusted client embedding array (a | N | [FB] |
| `search/soundness-hnsw-panic-corrupts-ind` | soundness | src/search/ann.rs:333-341 | assert! in notify_insert panics with permanently-corrupt HNSW index | N | [V] |
| `mcp/soundness-dump-path-symlink` | soundness | memory-engine-mcp/src/tools/mod.rs:1062-1066 | Dump path security check vulnerable to symlink in final filename compo | N |  |
| `scope/soundness-no-cycle-guard` | soundness | src/scope/tree.rs:61-68, 147-168 | ancestors and path_for_id loop forever on cyclic parent links | N | [V] |
| `store/explicit-counter-loop` | style | src/store/scopes.rs:116 | explicit_counter_loop denied error | Y | [V] |
| `archive/inline-search-no-tests` | testing | src/archive/search.rs (117 lines, no #[cfg(t | search.rs has zero inline tests — public search_archives is entirely u | N |  |
| `archive/pak-missing-error-path-tests` | testing | src/archive/pak.rs:58-66 (read_pak), pak.rs: | read_pak error paths and decompression-bomb cap are not tested | N |  |
| `bootstrap/testing-max-turns-assertion-bu` | testing | tests/bootstrap_test.rs:201-205 | max_turns test asserts the pre-truncation turn count, proving nothing | N |  |
| `bootstrap/testing-missing-persistence-cl` | testing | tests/bootstrap_test.rs | PersistenceClassifier path never exercised in integration tests | N |  |
| `bootstrap/testing-savepoint-rollback-not` | testing | tests/bootstrap_test.rs | Savepoint rollback on mid-pipeline failure is never tested | N |  |
| `cli/testing-no-inline-tests-output-db-em` | testing | memory-engine-cli/src/output.rs, memory-engi | Zero inline tests in output.rs, db.rs, and embedding.rs | N |  |
| `cli/testing-no-integration-record-outcom` | testing | memory-engine-cli/tests/cli_integration.rs,  | record-outcome and outcome-counts subcommands have zero integration te | N |  |
| `cli/testing-export-fully-blocked-no-acti` | testing | memory-engine-cli/tests/cli_integration.rs:3 | The only export integration test is permanently #[ignore] due to a lib | N |  |
| `consolidation/testing-no-inline-tests-or` | testing | src/consolidation/mod.rs:35-78 | consolidate() orchestrator has no #[cfg(test)] module | N |  |
| `consolidation/testing-no-transaction-rol` | testing | src/consolidation/mod.rs:27,50,68 | Transaction rollback on SummaryGenerator failure is untested | N |  |
| `core-root/testing-async-coverage-gap` | testing | src/async_engine.rs:91-543 | AsyncMemoryEngine has 6 tests covering ~25+ public methods | N |  |
| `embed/testing-public-api-untested-in-cra` | testing | memory-engine-embed/src/http.rs:116-248 | Public API embed() and embed_batch() have no inline tests within this  | N |  |
| `engine/integration-dormant-no-test` | testing | src/engine/dormant.rs + tests/ | `sample_dormant` has zero integration test coverage | N |  |
| `engine/integration-lineage-no-test` | testing | src/engine/lineage.rs + tests/ | Lineage API (`record_lineage`, `get_provenance`, `get_full_lineage`, ` | N |  |
| `engine/integration-dream-cycle-no-test` | testing | src/engine/cognitive.rs + tests/ | `run_dream_cycle` and `record_insight` have zero integration test cove | N |  |
| `engine/fuzz-snapshot-binary-parser` | testing | src/engine/snapshot.rs:206-240 | No fuzz target for `load_from_file` binary parser (untrusted file inpu | N |  |
| `forgetting/testing-missing-graph-edge-ca` | testing | src/forgetting/policy.rs:120-131 | Graph edge cascade on prune is never tested | N |  |
| `graph/testing-remove-node-stale-index` | testing | src/graph/memory_graph.rs:82-87 | remove_node is untested and its petgraph swap_remove semantics silentl | N |  |
| `inspect/testing-expired-invalidated-stat` | testing | src/inspect/explain.rs:87-95 | FactState::Expired and FactState::Invalidated branches have zero test  | N |  |
| `inspect/testing-dump-live-db-guard` | testing | src/inspect/dump.rs:196-206 | dump_sqlite live-database safety guard has no test | N |  |
| `inspect/testing-restore-managed-config-e` | testing | src/inspect/restore.rs:351-355 | MANAGED_CONFIG_KEYS exclusion during restore has no test | N |  |
| `mcp/testing-inline-tools-no-unit-tests` | testing | memory-engine-mcp/src/tools/mod.rs | 1387-line tools module has no #[cfg(test)] block — private parsing hel | N |  |
| `mcp/testing-missing-integration-six-tool` | testing | memory-engine-mcp/tests/ | Six dispatch-level tools have zero integration test coverage | N |  |
| `mcp/testing-dos-unbounded-inputs-unteste` | testing | memory-engine-mcp/src/tools/mod.rs:1131-1203 | DoS-relevant unbounded-input code paths have no test coverage | N |  |
| `mcp/testing-dump-path-traversal-rejectio` | testing | memory-engine-mcp/src/tools/mod.rs:1056-1072 | Security-critical path-traversal rejection in handle_dump_state is not | N |  |
| `mcp/testing-no-fuzz-target-dispatch` | testing | memory-engine-mcp/src/tools/mod.rs:377-419 | No fuzz target exists for tools::dispatch — primary untrusted-input en | N |  |
| `scope/testing-ancestors-inherited-query-` | testing | tests/eval/conformance/scope_isolation.rs (m | ScopeQuery::Ancestors and ScopeQuery::Inherited have no integration te | N |  |
| `scope/testing-snapshot-roundtrip-missing` | testing | src/scope/tree.rs:171-190 (to_snapshot / fro | ScopeTree::to_snapshot / from_snapshot have no roundtrip test | N |  |
| `search/testing-expired-probe-e2e` | testing | src/search/query.rs:200-203, src/engine/quer | `include_expired_probe` contract has no end-to-end test | N |  |
| `search/testing-hybrid-temporal-postfilte` | testing | src/search/hybrid.rs:218-230 | Bi-temporal post-filter in `hybrid_search` never exercised at unit lev | N |  |
| `store/testing-stamp-surfaced` | testing | src/store/facts.rs:473 | FactStore::stamp_surfaced has zero test coverage | N |  |
| `store/testing-list-dormant` | testing | src/store/facts.rs:198 | FactStore::list_dormant has no tests and captures Utc::now() internall | N |  |
| `store/testing-update-importance-score` | testing | src/store/facts.rs:564 | FactStore::update_importance_score is untested and missing NotFound gu | N |  |
| `store/testing-untested-list-variants` | testing | src/store/facts.rs:310,334,543,574,677 | Five FactStore query methods have no inline tests | N |  |
| `store/testing-edge-expire-silent` | testing | src/store/edges.rs:85 | EdgeStore::expire silently succeeds for nonexistent edge IDs | N |  |
| `store/testing-silent-json-swallow` | testing | src/store/activities.rs:218,src/store/checkp | row_to_activity and row_to_checkpoint silently discard JSON parse erro | N |  |

## Medium / Low / Info — bucketed by category

| Category | Medium | Low | Info | Auto-fixable |
| --- | ---: | ---: | ---: | ---: |
| build-system | 0 | 0 | 1 | 1 |
| correctness | 18 | 15 | 2 | 15 |
| design | 41 | 22 | 5 | 26 |
| documentation | 30 | 54 | 11 | 40 |
| mock-test-infra | 1 | 0 | 0 | 0 |
| modern-rust | 3 | 11 | 4 | 15 |
| performance | 12 | 19 | 0 | 12 |
| refactoring | 16 | 18 | 1 | 15 |
| security | 13 | 14 | 5 | 11 |
| soundness | 3 | 0 | 0 | 1 |
| style | 1 | 28 | 3 | 26 |
| supply-chain | 1 | 0 | 1 | 0 |
| testing | 84 | 55 | 5 | 27 |

## All findings by module

| Module | blocker | critical | high | medium | low | info | total |
| --- | --: | --: | --: | --: | --: | --: | --: |
| store | 0 | 0 | 10 | 23 | 21 | 6 | 60 |
| bootstrap | 0 | 0 | 4 | 15 | 19 | 4 | 42 |
| engine | 1 | 1 | 7 | 13 | 15 | 4 | 41 |
| mcp | 0 | 0 | 10 | 13 | 16 | 2 | 41 |
| inspect | 0 | 0 | 5 | 21 | 14 | 1 | 41 |
| core-root | 0 | 0 | 5 | 17 | 17 | 1 | 40 |
| cli | 0 | 1 | 5 | 17 | 14 | 2 | 39 |
| search | 0 | 0 | 7 | 14 | 15 | 2 | 38 |
| archive | 0 | 0 | 7 | 12 | 14 | 2 | 35 |
| consolidation | 0 | 0 | 5 | 10 | 17 | 2 | 34 |
| embed | 0 | 0 | 6 | 14 | 13 | 1 | 34 |
| scope | 0 | 0 | 3 | 10 | 10 | 4 | 27 |
| conflict | 0 | 0 | 0 | 13 | 11 | 1 | 25 |
| pool | 0 | 0 | 1 | 7 | 14 | 1 | 23 |
| forgetting | 0 | 0 | 1 | 8 | 9 | 2 | 20 |
| resume | 0 | 0 | 1 | 10 | 8 | 0 | 19 |
| graph | 0 | 0 | 3 | 4 | 8 | 1 | 16 |
| workspace | 0 | 0 | 2 | 1 | 0 | 0 | 3 |
| sc | 0 | 0 | 0 | 1 | 0 | 2 | 3 |
| docs | 0 | 0 | 0 | 0 | 1 | 0 | 1 |

## Refactoring backlog (HEAVY design/refactoring, non-auto-fixable: 54)

| ID | Sev | Location | Title |
| --- | --- | --- | --- |
| `store/design-schema-ddl-divergence` | high | src/store/schema.rs:686-687 (migrate_v8_ | idx_activities_dedup index column count diverges between fresh ins |
| `archive/design-archivepak-no-version-c` | high | src/archive/pak.rs:58-67, src/archive/ty | `ArchivePak` has no constructor and `read_pak` does not validate ` |
| `cli/design-stringly-typed-fact-type-tr` | high | memory-engine-cli/src/commands/add_fact. | Three parallel fact-type converters mapping `FactType` variants: t |
| `cli/maintainability-near-zero-test-cov` | high | memory-engine-cli/src/ | 11 of 12 source files lack any inline tests — critical paths entir |
| `consolidation/design-usize-max-sentine` | high | src/consolidation/dedup.rs:43-45, src/co | Magic usize::MAX sentinel encodes 'skipped' status in numeric retu |
| `consolidation/design-pseudo-facts-leak` | high | src/consolidation/global.rs:34-57 | global_integration constructs fake Fact structs from Summary to sa |
| `core-root/design-glob-reexport-error` | high | src/lib.rs:52 | Glob re-export of error module creates implicit public API surface |
| `core-root/design-glob-reexport-types` | high | src/lib.rs:58 | Glob re-export of types module exposes ~30 types without an explic |
| `core-root/design-importance-field-ambi` | high | src/types.rs:133-139 | `Fact` has two importance fields with near-identical names but dif |
| `embed/design-duplicated-http-plumbing` | high | memory-engine-embed/src/http.rs:116-181, | HTTP request/response plumbing duplicated verbatim between `embed` |
| `inspect/design-impl-submodules-overcal` | high | src/inspect/mod.rs:6-11 | Implementation submodules exposed as `pub` despite no external cal |
| `mcp/design-god-module-tools` | high | /home/mroynard/dev/memory-engine/memory- | God module: 1386-line flat file mixing tool schema, dispatch, and  |
| `pool/design-inmemory-reentrant-deadloc` | high | src/pool/connection_pool.rs:187-192 | read() in in-memory mode acquires write_conn.lock(), creating a de |
| `resume/design-impure-default-now` | high | src/resume/context.rs:29-41 | ResumeConfig::Default captures wall-clock time — impure and stale- |
| `store/design-god-module-schema` | high | src/store/schema.rs (2491 lines) | schema.rs is a 2491-line god module mixing DDL, migrations, config |
| `store/design-stringly-typed-relation-t` | high | src/store/edges.rs:21,49 (and types.rs N | relation_type is a bare String throughout EdgeStore — no newtype o |
| `archive/design-search-unbounded-accumu` | medium | src/archive/search.rs:41-108 | `search_archives` accumulates all matching facts across all paks b |
| `bootstrap/design-too-many-args-no-cont` | medium | src/bootstrap/mod.rs:40-50, 120-132, 279 | Three functions with 9–11 arguments suppressed by `#[allow(clippy: |
| `bootstrap/design-duplicate-temp-fact-c` | medium | src/bootstrap/mod.rs:207-228 | Full `Fact` struct constructed as scratch pad to call `Persistence |
| `consolidation/design-cluster-threshold` | medium | src/consolidation/cluster.rs:11, src/tra | Cluster similarity threshold is a hardcoded constant while dedup t |
| `consolidation/maintainability-duplicat` | medium | src/consolidation/dedup.rs:30, src/conso | Safety cap constant duplicated in dedup and cluster with inconsist |
| `core-root/design-stringly-typed-error-` | medium | src/error.rs:5-53 | Ten error variants carry only `String`, preventing structured matc |
| `core-root/design-stringly-typed-outcom` | medium | src/types.rs:534,551,577 | `outcome_class` is `String` across three types where an enum would |
| `embed/design-untyped-response-format` | medium | memory-engine-embed/src/http.rs:145-158, | Response format detected via if-let chain on `serde_json::Value` i |
| `engine/design-archive-full-table-scan` | medium | src/engine/archive.rs:151-172 | select_archive_candidates() materialises all facts and edges then  |
| `engine/design-record-lineage-atomicity` | medium | src/engine/lineage.rs:17 | pub fn record_lineage() exposes non-atomic lineage insertion — cal |
| `forgetting/design-policy-types-misplac` | medium | src/traits.rs:200-293 | ForgetPolicy and PruneStats are concrete structs housed in traits. |
| `graph/design-stringly-typed-relation-t` | medium | src/graph/memory_graph.rs:17 | EdgeData.relation_type is stringly-typed; no shared enum enforces  |
| `inspect/design-manual-json-header-in-s` | medium | src/inspect/dump.rs:43-73 | JSON object header hand-assembled with raw `write!` instead of a s |
| `mcp/design-stringly-typed-enums` | medium | /home/mroynard/dev/memory-engine/memory- | Hand-rolled string→enum parsers duplicate what serde already provi |
| `mcp/design-dump-state-path-canonicaliz` | medium | /home/mroynard/dev/memory-engine/memory- | handle_dump_state path restriction canonicalizes the parent but no |
| `mcp/design-bootstrap-no-size-limit` | medium | /home/mroynard/dev/memory-engine/memory- | handle_bootstrap_session accepts unbounded jsonl_data string — pot |
| `pool/design-in-memory-discriminant-sen` | medium | src/pool/connection_pool.rs:25, 132, 188 | read_pool_size == 0 is used as an implicit 'in-memory mode' sentin |
| `pool/design-writeasreadguard-semantics` | medium | src/pool/connection_pool.rs:52-60 | WriteAsReadGuard name implies read-only but exposes a fully-writab |
| `resume/design-kb-stubs-yagni` | medium | src/resume/context.rs:54-56, 104-105 | kb_stubs placeholder in public struct leaks internal roadmap into  |
| `resume/maintainability-missing-config-` | medium | src/resume/context.rs:12-27 | ResumeConfig has no validate() despite containing unconstrained fl |
| `scope/design-roundtrip-break-root` | medium | src/scope/tree.rs:44-58,147-168 | `path_for_id(root)` returns `"/"` which `resolve_path` cannot pars |
| `scope/design-validation-asymmetry` | medium | src/scope/tree.rs:44-58 | `resolve_path` accepts inputs that `ScopeStore::ensure_path` would |
| `search/design-ann-n-plus-one` | medium | src/search/ann.rs:279-287 | HnswStrategy::search issues two DB round-trips per candidate (N+1  |
| `search/design-hybrid-function-length` | medium | src/search/hybrid.rs:136-255 | hybrid_search is a 120-line function mixing three distinct phases |
| `search/design-searchquery-struct-liter` | medium | src/search/hybrid.rs:36-48 | SearchQuery is a fully-public struct constructed via struct-litera |
| `search/design-module-visibility` | medium | src/search/mod.rs:4-9 | All search submodules are pub, exposing impl-internal paths to dow |
| `store/design-wrong-error-variant-parse` | medium | src/store/facts.rs:29, src/store/events. | str_to_fact_type and str_to_event_type return MemoryError::NotFoun |
| `store/design-too-many-args-archive-ins` | medium | src/store/archive_manifest.rs:26-39 | ArchiveManifestStore::insert has 10 positional parameters, suppres |
| `cli/refactoring-ingest-from-reader-len` | medium | memory-engine-cli/src/commands/batch_ing | `ingest_from_reader` at 96 LOC mixes line-level parsing, validatio |
| `conflict/refactoring-manual-newfact-to` | medium | src/conflict/temporal.rs:48-67 | Manual NewFact→Fact field-copy should be a From impl or constructo |
| `conflict/refactoring-transaction-arm-d` | medium | src/conflict/temporal.rs:78-172 | Three CrudDecision arms repeat the open-tx / expire / edge-insert  |
| `core-root/refactoring-async-engine-boi` | medium | src/async_engine.rs:91-543 | 28 identical `spawn_blocking` wrappers with no structural abstract |
| `core-root/refactoring-types-god-module` | medium | src/types.rs:1-868 | `types.rs` is a 868-line heterogeneous god module with no internal |
| `core-root/refactoring-promotion-proven` | medium | src/types.rs:205-219 | `PromotionProvenance.lineage_id` is a phantom field: `skip_seriali |
| `engine/refactoring-classifier-temp-fac` | medium | src/engine/ingest.rs:56-79 and 176-200 | Identical 15-field temporary Fact construction duplicated in add_f |
| `inspect/refactoring-restore-snapshot-g` | medium | src/inspect/restore.rs:200-396 | `restore_snapshot_into` is a 196-LOC god function; extract per-ent |
| `search/refactoring-hnsw-build-duplicat` | medium | src/search/ann.rs:94-138 (build_from_db) | build_from_db and from_snapshot share ~80% identical HNSW-init bod |
| `store/refactoring-inconsistent-scope-f` | medium | src/store/facts.rs (json_each at list_pi | Two incompatible IN-list strategies for scope_id filtering in Fact |
