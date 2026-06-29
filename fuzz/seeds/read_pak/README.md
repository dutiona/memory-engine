# `read_pak` seed corpus

The `read_pak` fuzz target drives `memory_engine::fuzz_seam::read_pak`, a
two-layer parser: a `zstd` frame wrapping `serde_json::from_reader::<ArchivePak>`.
Without a seed, the fuzzer almost never synthesizes the 4-byte zstd magic
(`28 b5 2f fd`), so coverage plateaus at the lazy-`Decoder` error path and never
reaches the JSON deserializer or the version-validation branches. These seeds
start the fuzzer **past the zstd-magic wall** with a fully valid `.pak`, so
mutations explore both the zstd frame and the `ArchivePak` / `Fact` / `Edge`
JSON structure.

Measured coverage on these seeds vs. a lone non-zstd byte (libFuzzer
`-runs=0` startup replay): **824 edges / 1112 features** with the seeds, vs.
**91 edges / 92 features** for a single non-zstd byte — a ~9x edge gain. The
seeds are load-bearing, not decorative.

## Why `seeds/`, not `corpus/`

`fuzz/.gitignore` ignores `corpus/` (and `artifacts/`, `coverage/`): the
fuzzer-grown corpus is throwaway, machine-specific output that must not be
committed. **Seeds are source** — the hand-authored inputs that bootstrap a
fresh run — so they live in this tracked `fuzz/seeds/read_pak/` directory
instead. cargo-fuzz takes any number of extra input directories on the command
line, so point the run at this dir directly:

```sh
cargo +nightly fuzz run read_pak fuzz/seeds/read_pak
```

(or copy `fuzz/seeds/read_pak/*.pak` into `fuzz/corpus/read_pak/` once before a
campaign — libFuzzer merges both).

## The seeds

Both are zstd-compressed (level 3, matching `write_pak_and_hash`) JSON of an
`ArchivePak`:

- `empty.pak` — no facts/edges; the minimal happy path through both version
  checks.
- `populated.pak` — one `Fact` + one `Edge`, every field present; also
  exercises the nested `Fact`/`Edge` deserializers.

Both stamp `pak_version = 2` (`CURRENT_PAK_VERSION`) and
`engine_schema_version = 14` (`CURRENT_SCHEMA_VERSION`), so they read back as
`Ok` rather than tripping the forward-version-rejection branches.

## Regenerating

```sh
# JSON payload matching `ArchivePak` (PascalCase `fact_type`, RFC3339 timestamps):
cat > pak.json <<'JSON'
{"pak_version":2,"engine_schema_version":14,"embed_dim":3,
 "created_at":"2026-01-01T00:00:00Z","facts":[],"edges":[]}
JSON

# zstd level 3 = the level write_pak_and_hash() uses:
zstd -3 -c pak.json > empty.pak
```

Alternatively, drop any real archive produced by the engine's archival path
(`write_pak_and_hash`) into this directory — it is already a valid `.pak`.
