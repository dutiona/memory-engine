#!/usr/bin/env bash
# cargo-gate-guard.sh — PreToolUse/Bash guard against false-green cargo gates.
#
# Blocks piping a `cargo {test,clippy,build,doc,nextest}` invocation through
# head/tail. That truncation hides RED results AND the pipe discards cargo's
# exit code (PIPESTATUS reflects the pager, not cargo) → a FALSE-GREEN gate.
# This is the #1 recurring super-qa failure class in this repo (see the
# `qa-sweep` skill, Class 1, trap 1: `head -N` once masked 8 RED insta
# snapshots; a `tail` + PIPESTATUS capture came back empty).
#
# ── WIRING (local only) ──────────────────────────────────────────────────────
# `.claude/settings.json` is gitignored here, so this hook protects only the
# local checkout — the committed CLAUDE.md / AGENTS.md / GEMINI.md remain the
# guardrail that reaches every clone. To activate, add to `.claude/settings.json`
# (it coexists additively with the global rtk hook):
#
#   {
#     "hooks": {
#       "PreToolUse": [
#         { "matcher": "Bash",
#           "hooks": [
#             { "type": "command",
#               "command": "$CLAUDE_PROJECT_DIR/utils/scripts/hooks/cargo-gate-guard.sh" }
#           ] }
#       ]
#     }
#   }
#
# ── PROTOCOL ─────────────────────────────────────────────────────────────────
# Reads the PreToolUse JSON on stdin; on a match it prints the reason to stderr
# and exits 2 (the portable "block this tool call" signal). Otherwise exits 0
# (allow). It never mutates the command.

set -uo pipefail

input="$(cat)"

# Pull the command field. Prefer jq; fall back to a best-effort sed parse so the
# guard still works on a box without jq.
if command -v jq >/dev/null 2>&1; then
	cmd="$(printf '%s' "$input" | jq -r '.tool_input.command // empty' 2>/dev/null)"
else
	# `[^"]*` (not greedy `.*`) stops at the first closing quote of the value, so a
	# later JSON key cannot bleed into the captured command. A command containing an
	# escaped quote (`\"`) is the jq path's job; this fallback is best-effort.
	cmd="$(printf '%s' "$input" | sed -n 's/.*"command"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
fi

[ -z "${cmd:-}" ] && exit 0

# Best-effort heuristic (NOT a full bash parser). Rather than matching the cargo
# subcommand and the `| head/tail` independently anywhere in the string — which
# false-positives on `cargo build && ls | head` and `echo "cargo test | head"`,
# and is bypassed by `|&` / a newline / a path-prefixed `head` — we split the
# command into pipeline SEGMENTS on the logical separators `;`, `&&`, `||`, then
# block only a segment that is BOTH a cargo gate invocation AND pipes into
# head/tail. This scopes the pipe to its own pipeline and anchors `cargo` to the
# segment start (after optional `VAR=val` assignments or a bare wrapper like
# `env`/`command`/`nice`/`time`/`nohup`), so a quoted literal or a sibling command
# no longer trips it. grep is intentionally NOT blocked (it does not truncate).
# Residual, accepted misses (it is a heuristic, not a bash parser): a subshell
# `( cargo test | head )`, an arg-taking wrapper (`timeout 60 cargo … | head`,
# `stdbuf -oL head`), and full indirection (`sh -c '…' | head`, shell aliases).

# Replace quoted spans with spaces so a separator or pipe INSIDE a quoted argument
# (e.g. `cargo test --features "a;b" | head`) cannot confuse the split below. Pure
# bash, no deps; errs toward masking (a false negative is at worst the no-guard
# baseline). Escaped quotes inside quotes are not tracked — acceptable for a guard.
mask_quotes() {
	local s="$1" out="" q="" c i
	for ((i = 0; i < ${#s}; i++)); do
		c="${s:i:1}"
		if [ -z "$q" ]; then
			case "$c" in '"' | "'") q="$c" out+=" " ;; *) out+="$c" ;; esac
		elif [ "$c" = "$q" ]; then
			q="" out+=" "
		else
			out+=" "
		fi
	done
	printf '%s' "$out"
}

# Normalize newlines/tabs to spaces (a newline-separated pipe would dodge a
# line-oriented grep), mask quoted spans, then split on the logical separators.
norm="${cmd//$'\n'/ }"
norm="${norm//$'\t'/ }"
norm="$(mask_quotes "$norm")"
norm="${norm//';'/$'\n'}"
norm="${norm//'&&'/$'\n'}"
norm="${norm//'||'/$'\n'}"

# A cargo gate at the segment start: optional `VAR=val` assignments and/or a bare
# wrapper (env/command/nice/time/nohup), then `cargo [+toolchain] <gate>`. The gate
# is closed by whitespace, a pipe, or end — `([[:space:]|]|$)`, NOT a general
# non-alnum boundary, so a hyphenated subcommand like `build-docs`/`doc-open` is
# NOT mistaken for the `build`/`doc` gate (`\b` is also a non-POSIX GNU extension).
GATE_RE='^[[:space:]]*((env|command|nice|time|nohup|[A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*)[[:space:]]+)*cargo[[:space:]]+(\+[^[:space:]]+[[:space:]]+)?(test|clippy|build|doc|nextest)([[:space:]|]|$)'
# A pipe (`|` or `|&`) into head/tail, allowing a leading path (`/usr/bin/head`).
# Over-matching here is safe (it only ever blocks more), so the looser boundary stays.
PIPE_RE='\|&?[[:space:]]*([^[:space:]|]*/)?(head|tail)([^[:alnum:]_]|$)'

while IFS= read -r seg; do
	[ -z "$seg" ] && continue
	printf '%s' "$seg" | grep -Eq "$GATE_RE" || continue
	if printf '%s' "$seg" | grep -Eq "$PIPE_RE"; then
		cat >&2 <<'EOF'
cargo-gate-guard: refusing to pipe a cargo gate through head/tail.

Truncation hides RED results, and the pipe discards cargo's exit code
(PIPESTATUS reflects head/tail, not cargo) → a FALSE-GREEN gate.

Run it unpiped, or redirect to a file and read the file:
  cargo test --workspace --all-features > /tmp/cargo-gate.log 2>&1; tail -40 /tmp/cargo-gate.log
EOF
		exit 2
	fi
done <<<"$norm"

exit 0
