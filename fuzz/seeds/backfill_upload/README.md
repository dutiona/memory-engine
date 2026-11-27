# `backfill_upload` seed corpus

One seed per checked-in upload input (#1120), in the target's input format: eleven `0x00`-separated header parts
(`ME-Backfill-Kind`, `ME-Backfill-Mode`, `ME-Backfill-Source`, `ME-Backfill-Mtime`, `ME-Backfill-Preview`,
`ME-Client-Version` and the five `ME-Project-*` headers), then the body. An empty part takes the target's default, so
each seed sets only its kind and its source, whose last segment is the fixture's file name (a memory file is named by
it, so `MEMORY.md` and the stub reach their own skips):

- `transcript-*`: the two transcripts of `crates/memory-engine-mcp/tests/fixtures/claude-code-transcripts/`, and
  `transcript-future`, the main one dated 2099 instead of 2026: a machine whose clock ran ahead (#1153);
- `memory-file-*`: the seven memory files of `crates/memory-engine-mcp/tests/fixtures/claude-code-memory/`;
- `claude-mem-*`, `claude-memory-*`: each JSON Lines file `memory-engine-plugin-export` writes from
  `crates/memory-engine-mcp/tests/fixtures/plugin-stores/`, the plugin-store export input.

Each is a dry run: the target replays a dry run that answers 200 as the write of its preview, so the commit path runs
too. Seeds are source, so they live here, and the run names them after the corpus directory: libFuzzer writes every
new input to the first directory it is given. That directory is `fuzz/target/corpus/backfill_upload`, under the
directory cargo marks with `CACHEDIR.TAG`, so the fuzzer's output goes with the build output; the empty
`fuzz/artifacts/backfill_upload/` cargo-fuzz always creates is removed after a clean run, and a crash's artifact
stays there to be read (#1161).

```sh
cargo +nightly fuzz run backfill_upload fuzz/target/corpus/backfill_upload fuzz/seeds/backfill_upload -- -create_missing_dirs=1 -max_total_time=60 -timeout=10 && find fuzz/artifacts -type d -empty -delete
```

Regenerating, from the repository root, after a fixture changes:

```sh
find fuzz/seeds/backfill_upload -type f ! -name README.md -delete
cargo build -p memory-engine-mcp --bin memory-engine-plugin-export
python3 -I - target/debug/memory-engine-plugin-export <<'PY'
import os, sqlite3, subprocess, sys, tempfile

export = sys.argv[1]
fx = 'crates/memory-engine-mcp/tests/fixtures'
out = 'fuzz/seeds/backfill_upload'
os.makedirs(out, exist_ok=True)

def seed(name, kind, source, body):
    # kind, mode, source, mtime, preview, client version, five project headers, then the body;
    # an empty part takes the target's default.
    parts = [kind.encode(), b'', source.encode()] + [b''] * 8
    with open(os.path.join(out, name), 'wb') as f:
        f.write(b'\0'.join(parts + [body]))

project = 'projects/-work-fixture-p/'
main = '52c6c7b7-c890-43ec-81a5-a1cf99d3cc84'
for rel, name in [(f'{main}.jsonl', 'main'), (f'{main}/subagents/agent-a3c91f2.jsonl', 'subagent')]:
    with open(f'{fx}/claude-code-transcripts/-work-fixture-p/{rel}', 'rb') as f:
        seed(f'transcript-{name}', 'transcript', project + rel, f.read())
with open(f'{fx}/claude-code-transcripts/-work-fixture-p/{main}.jsonl', 'rb') as f:
    seed('transcript-future', 'transcript', f'{project}{main}.jsonl',
         f.read().replace(b'2026-03-19', b'2099-03-19'))

for file in sorted(os.listdir(f'{fx}/claude-code-memory')):
    with open(f'{fx}/claude-code-memory/{file}', 'rb') as f:
        seed(f'memory-file-{file}', 'memory-file', f'projects/-work-fixture-p/memory/{file}', f.read())

for kind in ['claude-mem', 'claude-memory']:
    with tempfile.TemporaryDirectory() as tmp:
        store = os.path.join(tmp, 'store.db')
        db = sqlite3.connect(store)
        with open(f'{fx}/plugin-stores/{kind}.sql') as f:
            db.executescript(f.read())
        db.close()
        dest = os.path.join(tmp, 'out')
        subprocess.run([export, kind, store, dest], check=True, stdout=subprocess.DEVNULL)
        for root, _, files in os.walk(dest):
            for file in sorted(f for f in files if f.endswith('.jsonl')):
                path = os.path.join(root, file)
                rel = os.path.relpath(path, dest)
                with open(path, 'rb') as f:
                    seed(f'{kind}-{rel.replace("/", "-")}', kind, rel, f.read())
PY
```
