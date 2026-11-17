# Clean-slate review of the successor plan (v2), round 1

Reviewer: a fresh Claude Opus subagent with no conversation context (read-only), the fallback after agy
`claude-opus-5-5-high` answered 429 (quota, resets in about 3 h).

Altitude report SHA-256: 23bf35fea61861112c840c8916adccedd4cc8ae7c2cd76aea9ab41bfc40d1ac7

Verdict: approve once the two MEDIUM findings are fixed; no BLOCKER or HIGH. The delta is bounded and applied
consistently, and D1's recommendation (B, one pooled vector per fact) holds against the code. The altitude report
checks out (plan `4ff4ce8a…`, predecessor `248b4a41…`, 729 lines, 66,865 bytes, 19 items); a complete successor,
not an amendment, is right.

- [MEDIUM] Item 5 / Open question 13: v2 drops a v1 property silently. v1 cut on the self-identifying
  `exceed_context_size_error` 400 whatever the label and with no declared M; v2 splits only with a declared M. The
  recorded body carries `n_ctx`, the server's own number, so `n_ctx − 1` can serve as the chunk limit when M is
  undeclared. Keep that fallback, or list the dropped property.
- [MEDIUM] Item 5: a `/tokenize` failure classified "like an embed call" is wrong for a missing endpoint: a 404/405
  becomes `Failed` (retried forever), a 400/422 `InputRejected` (failed for good). KB treats 400/404/405/422, or a
  body with no `tokens` list, as a configuration error (`embeddings.py` `token_counter`). Fall back to the byte
  bound with a `warn`, or pause as an alarm; test it.
- [LOW] `split_to_limit` returns `Vec<&str>`, but pooling weights by each piece's count: return `(&str, usize)`;
  KB's `_fit` does not always end on a counted cut, so the port must record one.
- [LOW] Untested rules: a counted 400 after a fitting count; MRL with pooling; a zero or non-finite pooled norm
  (`mrl_truncate` has that guard, `crates/memory-engine-embed/src/http.rs:281`); the rejoin and limit invariants
  suit a proptest (`proptest` is already a dev-dependency).
- [LOW] Item 10: `TestServer` hardcodes provider `"tei"` with no served artifact
  (`crates/memory-engine-mcp/tests/common/mod.rs:126`), so the over-length HTTP test cannot be expressed; it needs
  a constructor with `llama.cpp`, a declared limit and a `/tokenize` responder.
- [LOW] Large facts: up to `MAX_PAYLOAD_BYTES` = 1 MiB (`crates/me-types/src/limits.rs:32`) is about 125 pieces
  and about 2,000 counting probes plus 125 sequential embeds in one `embed`; one transient 5xx restarts the fact.
  State it as an accepted cost, or place cuts with `/tokenize`'s `with_pieces`.
- [LOW] D1: A also needs a space key and a staged copy-swap (`fact_vectors (fact_id, space_id)`,
  `crates/me-backend-sqlite/src/store/schema/ddl.rs:155-160`), which strengthens B; note that Open question 1's
  follow-up is where A lands; B's size is a module plus a counting client, not "one function"; hybrid recall's
  full-text side still finds a detail the pooled vector dilutes.
- [LOW] Port fidelity: KB gallops from the aim, not from `limit`; KB indexes by code points (the port gallops in
  characters mapped to byte offsets); KB's `/tokenize` URL keeps a path prefix (`removesuffix("/v1")`), the plan's
  "endpoint origin" drops it.
- [LOW] `embed_query` "inherits the rule" never triggers (`MAX_RECALL_QUERY_BYTES` = 1024 plus the instruction stays
  under the no-count bound); say so.
- [LOW] Documentation: `crate-layout.md` lacks `chunk.rs`; `mcp-server.md:98` should say `max_input_tokens` sets
  the chunk limit; `HttpEmbeddingProvider::embed`'s rustdoc ("via one HTTP POST", `http.rs:449`) changes; nothing
  durable records D1 (for example `docs/design/design-choices.md`).
- [LOW] Header omits the two added Constraints; the edited Outcome line was not re-wrapped.

Checked and correct: the deployed limit (2,048 counted with EOS, 2,047 text); the `bytes + 2` no-count bound;
B's "no reader changes" (every reader scores by cosine); the batch path keeps reconstruction working; no regression
for existing tests; Documentation, Testing and Verification present.

## Resolution

Round 1, applied to the successor (OpenCode's and Vibe's round-1 LOWs in the same revision):

- [MEDIUM] dropped property → taken: with no declared limit, llama-server's recorded 400 sets the chunk limit to
  `n_ctx − 1` and the input is chunked; `embed_batch` falls back per text on that answer (item 5, Open question 13).
- [MEDIUM] missing `/tokenize` → taken: 400/404/405/422 or a body without `tokens` falls back to the byte bound
  with one `warn`; test `over_length_missing_tokenize_falls_back_to_the_byte_bound`.
- [LOW] signature → returns `(&str, usize)`, the count recorded per piece.
- [LOW] tests → counted-400-after-fit, MRL with pooling, zero/non-finite pooled norm as an error, a proptest
  `chunk_invariants_hold`.
- [LOW] harness → item 10 names a `llama.cpp` constructor with a declared limit of 64 and a `/tokenize` responder.
- [LOW] large facts → stated as an accepted cost in item 5.
- [LOW] D1 → A's table is keyed by space and staged by reconstruction; the Later row names Open question 1; B's size
  restated; B's Recall row notes the full-text channel.
- [LOW] port fidelity → gallop from the aim, characters mapped to byte offsets, KB's URL rule (endpoint without its
  trailing `/v1/embeddings`).
- [LOW] `embed_query` → item 5 says queries never split.
- [LOW] docs → `crate-layout.md` names `chunk.rs`; `mcp-server.md`'s identity section; `design-choices.md` records
  D1; the `embed` rustdoc change is in item 5.
- [LOW] header → lists every changed section; the Outcome is re-wrapped.

# Round 2 (delta review, same reviewer)

Altitude report SHA-256: 73a248717f4c6ae5198da8f98d7400b45ce011d191f87d2aca1274656839e944

LGTM against `29441a37…`: every round-1 resolution holds (checked against the code and knowledge-base merge
`e95f0620`); nothing in the delta above LOW. Three [LOW]:
1. "sub-millisecond each" blurs the cited number; knowledge-base `docs/usage/ingesting-documents.md:334` says "a
   `/tokenize` call took 0.2 to 0.9 ms at the median (up to 3.8 ms)": over 2,000 calls the tail adds up to seconds.
2. Item 5 does not say whether the provider remembers the limit learned from the recorded 400, or the `/tokenize`
   fallback and its `warn`, or works them out per call (stateless is simpler: one rejected request per long fact).
3. The pooled-norm guard has no named test (a mock returning `v` and `−v` for two equal-weight pieces).
Checked, not a finding: with a declared limit, a counted 400 after a fitting count stays `InputRejected`; the
declared limit is identity (ADR-0015 §1), so a wrong declaration fails loudly in `status`.

## Resolution

Round 2 closes the successor's review: all three voices (OpenCode, Vibe, this reviewer) answered LGTM with LOW
findings only, so the orchestrator arbitrates the remainder without another round (review protocol, exit
condition 2). The plan text is unchanged; these LOWs are carried into implementation and checked in the plan
audit:

- This reviewer's 1 → the accepted-cost sentence is read with the cited figures (0.2 to 0.9 ms median, up to 3.8
  ms); the PR states them as written.
- This reviewer's 2 and Vibe's state note → stateless: the provider works the limit and the counter out per call
  (no interior mutability); the recorded 400 costs one rejected request per long fact; the `warn` is per fallback.
- This reviewer's 3 and OpenCode's 2 → the test `over_length_zero_norm_pool_is_an_error` (pieces embedded as `v`
  and `−v`) is added in item 5.
- OpenCode's 1 → item 10's `/tokenize` responder counts bytes / 2, so the 300-byte memory is three pieces at the
  limit of 64, and the test asserts at least two embed requests.
- OpenCode's 3 → the deployed server is reachable only from dutiona, not from the implementer's machine; the
  equality rests on knowledge-base#615's live check on the same llama.cpp build and GGUF, and a disagreement is
  `InputRejected`, visible in `status`. The PR names the infra check the owner can run after deploy.
- OpenCode's 4 → "b12048" is the llama.cpp release tag, not a host; the PR writes "llama.cpp build b12048".
- Vibe's `tracing` note → `memory-engine-embed` gains `tracing` (already in the workspace lock).
- Vibe's parse-location note → `send_request` returns the parsed error body beside the status, so item 5 reads
  the recorded shape from JSON, never from the capped string.
- Vibe's shortcut note → the `bytes + 2 <= limit` no-count shortcut gates every counter, `/tokenize` included.
- Vibe's label note → the recorded-400 rule is shape-based for any server; the label gates only `/tokenize`; Open
  question 13's wording is read that way.
