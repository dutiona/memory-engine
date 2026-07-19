# memory-engine — developer task runner.
#
# Wave 2 (#816 / S6, #942) legibility tooling: make the 18-crate link graph and
# the shipped symbol surface inspectable, so the decomposition is legible in
# practice — not just on paper. The crate split already delivered the structural
# value; these recipes let you *see* it.
#
# Requires `just` (https://github.com/casey/just):  cargo install just
# Individual recipes name any extra tooling they need and print an install hint
# when it is missing, so a fresh clone degrades gracefully instead of erroring.

# List the available recipes (runs on a bare `just`).
default:
    @just --list --unsorted

# The link graph of the shipped facade: the layered crate DAG (normal edges = what
# actually links, dev-deps excluded), each crate's contribution to binary size,
# and the native system libraries the final link pulls in. This is the "can I SEE
# the 18-crate split?" report the Wave 2 plan (#925 §8 / §11) calls for.

# Report the crate DAG, per-crate binary-size split, and native link libs.
linkmap:
    #!/usr/bin/env bash
    set -euo pipefail
    sep() { printf '\n── %s ──\n' "$1"; }

    sep "Crate DAG — facade \`memory-engine\`, normal edges (the ship graph)"
    cargo tree -p memory-engine --edges normal

    sep "Per-crate contribution to the release \`memory-engine-cli\` binary"
    if command -v cargo-bloat >/dev/null 2>&1; then
        cargo bloat --release --crates --bin memory-engine-cli || echo "  (cargo bloat failed)"
    else
        echo "  (skipped — install with: cargo install cargo-bloat)"
    fi

    sep "Native static libs the final link requires"
    # `--print native-static-libs` only emits for a `staticlib` crate-type, so override
    # it for this one rustc invocation (the facade's real crate-type is untouched). Use a
    # dedicated target-dir so this throwaway staticlib build does NOT invalidate the normal
    # release artifacts `just symbols` shares.
    # Branch on cargo's real exit status (not a diagnostic-regex heuristic) so a genuine
    # staticlib compile failure is never misreported as "no native libs"; match with a
    # here-string, never `echo "$nsl" | grep`, which can take SIGPIPE on a large payload.
    if nsl=$(cargo rustc -p memory-engine --release --quiet --lib --crate-type staticlib \
        --target-dir target/linkmap-nsl -- --print native-static-libs 2>&1); then
        grep -i 'native-static-libs:' <<<"$nsl" \
            || echo "  (staticlib built, but rustc emitted no native-static-libs line)"
    else
        echo "  (staticlib probe failed to compile — rerun verbose: cargo rustc -p memory-engine --release --lib --crate-type staticlib -- --print native-static-libs)"
    fi

# The shipped symbol surface: the *dynamic* symbols the release binaries EXPORT
# (`nm -D --defined-only`, Linux/ELF-specific). A leaf executable exports almost nothing —
# but a near-empty list means "provides no dynamic symbols", NOT "self-contained": the bins
# still dynamically link system libs (libssl / libc / … — see `ldd`) while statically
# linking the Rust crate graph. This exported surface only gets interesting for the deferred
# `dylib` ship mode (#994), where the facade's ABI would show up here.

# Report exported dynamic symbols of the release cli + mcp binaries.
symbols:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release --bins
    for art in target/release/memory-engine-cli target/release/memory-engine-mcp; do
        name=$(basename "$art")
        if [ ! -e "$art" ]; then printf '── %s: (not built) ──\n' "$name"; continue; fi
        # `nm -D` is Linux/ELF-specific; on a Mach-O/BSD `nm` it errors, which under
        # `set -euo pipefail` would hard-crash the recipe. Guard so it degrades gracefully.
        if ! nm -D --defined-only "$art" >/dev/null 2>&1; then
            printf '── %s: dynamic-symbol listing unsupported here (needs Linux/ELF `nm -D`) ──\n' "$name"
            continue
        fi
        count=$(nm -D --defined-only "$art" 2>/dev/null | wc -l | tr -d ' ')
        printf '── %s: %s exported dynamic symbols ──\n' "$name" "$count"
        nm -D --defined-only "$art" 2>/dev/null | awk '{print "   " $NF}' | sort | head -30 || true
        [ "${count:-0}" -gt 30 ] && echo "   … ($((count - 30)) more; full: nm -D --defined-only $art)" || true
    done

# The fuzz crate (`./fuzz`) is a DETACHED workspace: the repo-root
# `cargo build --workspace` never compiles it, yet it consumes the facade public
# API by path (memory-engine / -embed / -mcp). A facade API removal can break it
# with no default gate noticing (#993). This recipe IS that gate — it builds
# every fuzz target against the current facade. Needs a nightly toolchain +
# cargo-fuzz; both guarded below with an install hint.

# Build the detached fuzz targets against the facade API (#993 gate; needs nightly + cargo-fuzz).
fuzz-build:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! rustup toolchain list 2>/dev/null | grep -q nightly; then
        echo "error: needs a nightly toolchain — rustup toolchain install nightly" >&2
        exit 1
    fi
    if ! cargo +nightly fuzz --version >/dev/null 2>&1; then
        echo "error: needs cargo-fuzz — cargo install cargo-fuzz --locked" >&2
        exit 1
    fi
    cargo +nightly fuzz build
