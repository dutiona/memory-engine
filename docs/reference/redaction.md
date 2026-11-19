# Redaction

memory-engine redacts known secret formats on every write path before anything is stored, embedded or handed to a
consumer's callback (#1024). This page holds the rules, the paths they cover, the capture contract for tool calls and
the two counters. The code is `crates/memory-engine/src/redact/`.

## Rules

A detected secret is replaced by `[REDACTED:<rule>]`. Detectors run in priority order; an earlier one wins an
overlap.

| Priority | Rule | Matches |
|---|---|---|
| 0 | `denylist` | A literal of 4 bytes or more from the author's denylist file (`ME_REDACT_DENYLIST_FILE`). Bootstrap only; the server applies signatures only. |
| 1 | `machine-token` | `me_` and exactly 32 base62 characters, with no `[0-9A-Za-z_]` on either side (ADR-0020 §4). `me_types::Fact` is untouched. |
| 2 | provider prefixes | A prefix below at a token boundary, 20 bytes or more in all. |
| 3 | `connection-string-password` | The password of a `user:pass@` URL authority. |
| 4 | `hex-secret` | A run of 32 or more hex digits whose preceding word holds a secret key word (`key`, `token`, `secret`, `password`, …); in a JSON value, the member's key counts as that word. |
| 5 | `high-entropy` | A token of 20 or more characters of the secret charset at 4.3 bits per character or more. |

| Rule | Prefixes | Body |
|---|---|---|
| `aws-access-key` | `AKIA` | token |
| `github-pat` | `ghp_` | token |
| `github-fine-pat` | `github_pat_` | token |
| `github-token` | `gho_`, `ghu_`, `ghs_`, `ghr_` | base62 |
| `openai-key` | `sk-` | token |
| `google-api-key` | `AIza` | token |
| `slack-token` | `xoxb-`, `xoxp-`, `xapp-` | token |
| `private-key-header` | `-----BEGIN` | token |
| `jwt` | `eyJ` (needs a dot past the prefix) | token |
| `sendgrid` | `SG.` (needs a dot past the prefix) | token |
| `huggingface-token` | `hf_` | base62 |
| `tailscale-key` | `tskey-` | token |
| `gitlab-pat` | `glpat-` | token |
| `stripe-key` | `sk_live_`, `rk_live_` | base62 |
| `npm-token` | `npm_` | base62 |
| `pypi-token` | `pypi-` | token |
| `age-secret-key` | `AGE-SECRET-KEY-1` | token |
| `supabase-token` | `sbp_` | base62 |
| `docker-pat` | `dckr_pat_` | token |
| `digitalocean-token` | `dop_v1_` | base62 |

A token body holds alphanumerics and `. _ - + / =`; a base62 body holds ASCII alphanumerics only, because those
providers document one, so an identifier such as `npm_config_user_agent` stays readable.

Never redacted:

- Email addresses.
- Link contents (`scheme://` and the RFC 3986 characters but `'(),;`), query keys included: only rules 0, 1 and 3
  apply to a match that starts in a link's body, after its `://`, and its userinfo is not part of it (owner ruling,
  2026-11-17). A token glued to a URL with no separator (`AKIA…https://`, `ghp_…/https://`) starts before the body,
  so it is redacted.
- JSON keys: only string values are scanned.
- An event's capture context (`redact::PROVENANCE_KEY`, `project`) when it has exactly the shape the MCP server writes:
  its strings get the machine-token rule only, because the other rules misread ordinary paths (a dot-free root holding
  a UUID, a checkout named `sk-learn-tutorial-2024`) and scrubbing a fact the project key is derived from would merge
  distinct projects. Any other shape under that key is scrubbed normally.

**The fixpoint.** One pass can leave a remainder the next one catches, so write paths call `scrub`, which repeats
passes until one changes nothing, at most 8. A text that does not settle is refused with
`ConflictError::Redaction("redaction did not settle; nothing was stored")` and nothing is written; bootstrap skips such a
fact with a warning instead. Each scan is one pass plus binary searches, so its cost is bounded by the 1 MiB write limit.

Two memories that differ only by a redacted secret become the same text, so a forget's tombstone of one also keeps the
other out.

## Write paths

Every `MemoryEngine` write method checks its raw size, then (after its open and writable checks, so `ReadOnly` still
wins, #972) scrubs its content fields and stores the scrubbed copy: fact content and metadata, event payloads, forget
and move records, forget reasons, checkpoint summaries and metadata, recorded embedder errors, tool activities,
bootstrap's turns and facts, cycle deltas and consolidation summaries. `tests/redaction_write_paths.rs` reads every
public method of `crates/memory-engine/src/engine/` and fails until each is listed as gated, with its test, or exempt,
with its reason. A new write method must scrub its content and join the gated list.

Exempt, by reason:

- **No new text**: identifiers, numbers, edges, pins, lineage, decay pruning, archives, re-embedding stored text, and
  every read.
- **Restore and import**: `restore_json`, `restore_sqlite` and `memory-engine-cli import` load an operator-supplied ME
  snapshot as it is, because scrubbing its text would detach it from its stored vectors. A snapshot taken before this
  release restores its unredacted rows.
- **Not this store**: `record_insight` (the consumer's `InsightStream` stores it), `dump_state` (writes a file).

**Vectors the caller computed.** A write that brings its own vector is refused with `ConflictError::Redaction` when
redaction would change the text the vector was computed from: `add_fact_precomputed`, `resolve_conflict`'s candidate
and `DreamCtx::promote`. A clean write still succeeds, so the precomputed-vector API stays open; its metadata and
provenance, which are not embedded, are scrubbed.

**Cycle deltas.** `apply_cycle_report` skips an `AddFact` or `Synthesize` delta whose text is not clean and counts it
in `ApplyResult.blocked`, like a tombstone-blocked delta (#1023): the window is still marked, so a deterministic
proposer cannot livelock on it. Quarantine reasons, promotion provenance, the report's own `method_version` (kept in
the cycle history) and every metadata leaf are scrubbed. Every raw bound is checked before a delta is read: an
oversized delta is skipped as blocked, while an oversized quarantine reason or report label becomes an omission
marker, so the action still applies. The 1 MiB bound admits caller text: the engine's own annotations (a quarantine's
envelope, the cycle's `dream_cycle` stamp) may take a fact already at the limit a few dozen bytes past it.
Consolidation scrubs its `SummaryGenerator`'s output before embedding it. `LlmDreamCycle` embeds its proposer's
summaries below the facade, so `redact::ScrubbingProposer` wraps a proposer and scrubs each summary (dropping a group
whose summary exceeds the write limit or does not settle); `memory-engine-cli consolidate --backend llm` uses it. A
library caller that builds its own `LlmDreamCycle` without the wrapper embeds raw summaries, though the core still
refuses to store them.

## Tool calls

`record_activity` applies ADR-0020 §6's capture contract:

- **Redact, then cut.** The input and the result are scrubbed, then cut to 512 bytes of UTF-8, back to a character
  boundary (`redact::cut`). The cut is then settled: a cut inside a word can expose a match the whole word did not
  hold, so it is scrubbed and cut again until nothing changes, and a later scrub can never grow it past the cap. An input whose compact JSON fits stays JSON; a longer one is stored as a JSON string of its
  first 512 bytes. The cap applies to the decoded text; its JSON encoding may be longer. A promotion is built from the
  scrubbed result, so its own 200-byte cut also follows redaction. An `OutcomeClass::Other` text is redacted, then
  cut, like the result, on a stub too.
- **Deduplication** hashes the scrubbed, uncut input: two calls that differ only by a secret merge, and two long inputs
  that share their first 512 bytes stay two rows.
- **Over 1 MiB.** A raw input or result over the write limit is never scanned: an input is stored as
  `[omitted: over 1 MiB]` and never deduplicates (its hash is a per-call random stand-in, so no digest of the content
  is kept), and a result or an `Other` outcome is stored as `[omitted: <N> bytes]`; such a result is never promoted.
  The call is still recorded.
- **Stubs.** A call whose `touched_paths` names a secret is stored as a stub: its input becomes
  `{"sensitive_path": "<token>"}` (the token as spelled, scrubbed and cut), it keeps no result, is never promoted and
  never deduplicates with an ordinary call. Session, scope, time and outcome are kept.

`touched_paths` holds each path token as the harness adapter found it and as the call spelled it (ADR-0020 §11); the
request's `cwd` is the call's working directory. The core judges each token as given and joined lexically with `cwd`.
The server never sees the client's file system, so symlink targets are outside the contract.

The classifier, `redact::is_sensitive_path`, is a port of my-dotfiles' live command guard (`guard/internal/rules/names.go`,
`IsSecret` for a spelled path) with its name table as tests: `.env` and `*.env` files, key and certificate extensions,
known credential files (`.netrc`, `.pgpass`, `credentials.json`, …), home credential stores (`.aws/credentials`,
`.kube/config`, `.docker/config.json`, ME's own `.config/memory-engine/secrets/`, …), and data files named or filed as
secrets (`token.json`, `secrets/x.yaml`). Templates (`.example`, `.sample`, `.template`) are exempt. Case and the
Unicode folds a case-insensitive file system applies are folded. Where it departs from the guard, because ME decides
what persists rather than what an agent may read:

- the server knows no client's home, so a home-relative store matches as a whole path or after any `/`;
- every file under a `.ssh` directory but public keys, `config`, `known_hosts*` and `authorized_keys*` is a secret, and
  so are `/etc/shadow` and `/etc/gshadow`;
- the guard's exemptions for my-dotfiles' own policy files are not ported.

Because a relative token is also judged inside `cwd`, every data or extensionless file touched in a checkout whose path
holds a `secrets`, `credentials` or `vault` directory is stubbed too.

## Queries

The MCP `recall` tool scrubs its query before building its search terms and before embedding it, so a query quoting a
token searches for its placeholder, which is what a stored fact holds. `memory-engine-cli query` scrubs only the text
it embeds and keeps its FTS5 expression as typed (an unquoted placeholder is an FTS5 syntax error, and that text is
neither stored nor sent). No embedder receives a known token format from a query.

Upstream error bodies are quoted the same way: the embedder and proposer clients redact a body of at most 8 KiB whole,
then cut it to 1,000 bytes; a longer one is named by its status and size only.

## Counters

The store keeps two totals in its `config` table, shown by `memory-engine-cli status`
([CLI](cli-inspector.md)) and read by `memory_engine::read_redaction_counts`:

- `redact_count`: secrets redacted in text that was written. A forget's reason counts only when the fact was
  forgotten; a partial batch counts its stored records; a deduplicated activity counts the result it rewrote (its input
  only matched the row); an activity's input counts its findings past the 512-byte cut too. A bootstrap run adds its
  reported `secrets_redacted`, which also counts turns scrubbed before extraction.
- `stub_count`: activities stored as sensitive-path stubs, not counting their duplicates.

Uncounted: text scrubbed before the engine sees it (the MCP server's refusal errors and forget reasons,
`ScrubbingProposer`'s summaries, embedder and proposer error bodies) and queries, which are never stored. The totals
start at 0 on upgrade, and a crash between a write's commit and its count undercounts that write. A cycle's counts
are taken just before its apply, so a forget that commits between the two can count the findings of a delta the
apply then skips.

## The shared fixture

`tests/fixtures/redaction-cases.jsonl` pins the rule set shared with knowledge-base: one pass of `redact::redact`
reproduces every case, and both repositories carry the file byte for byte, each pinning its `sha256sum` in a test. A
rule change is a pair of PRs, memory-engine first, both quoting the same digest; an edit of one copy alone fails that
repository's pin.

## Residuals

- Rows stored before this release are not rewritten; today they are smoke data only.
- A caller can write the capture-context shape itself; its strings then get the machine-token rule only.
- A provider token typed into a project path or branch is stored, as one in a link is.
