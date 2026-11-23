# `lifecycle_event` seed corpus

One seed per checked-in Claude Code hook fixture
(`crates/memory-engine-mcp/tests/fixtures/claude-code-hooks/`), in the target's input format: the
five envelope headers, the five project headers, then the body, separated by `0x00`; an empty
part takes the target's default (an empty harness is `claude-code`). Without them the fuzzer
would have to synthesize a valid hook input before the adapter's mapping and the core's
capture contract run; with them it mutates from real bodies.

Seeds are source, not fuzzer output (`fuzz/.gitignore` ignores `corpus/`), so they live here
and the run names them on the command line, after the corpus directory: libFuzzer writes every
new input to the first directory it is given, so naming the seeds first would grow them with
generated files.

```sh
cargo +nightly fuzz run lifecycle_event fuzz/corpus/lifecycle_event fuzz/seeds/lifecycle_event -- -create_missing_dirs=1
```

Regenerating, from the repository root, after a fixture changes:

```sh
python3 -I - <<'PY'
import os
fx = 'crates/memory-engine-mcp/tests/fixtures/claude-code-hooks'
order = ['ME-Event-Id', 'ME-Event-Time', 'ME-Event-Seq', 'ME-Harness', 'ME-Client-Version',
         'ME-Project-Root', 'ME-Project-Dev-Root', 'ME-Project-Upstream-Url',
         'ME-Project-Origin-Url', 'ME-Project-Branch']
for name in sorted(os.listdir(fx)):
    d = os.path.join(fx, name)
    if not os.path.isdir(d):
        continue
    headers = dict(l.split(': ', 1) for l in open(os.path.join(d, 'headers')).read().splitlines() if l)
    parts = [headers.get(k, '').encode() for k in order]
    parts[3] = b''
    open(os.path.join('fuzz/seeds/lifecycle_event', name), 'wb').write(
        b'\0'.join(parts + [open(os.path.join(d, 'body'), 'rb').read()]))
PY
```
