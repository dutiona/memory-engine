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
	cmd="$(printf '%s' "$input" | sed -n 's/.*"command"[[:space:]]*:[[:space:]]*"\(.*\)".*/\1/p')"
fi

[ -z "${cmd:-}" ] && exit 0

# Best-effort heuristic (NOT a full bash parser). Rather than matching the cargo
# subcommand and the `| head/tail` independently anywhere in the string — which
# false-positives on `cargo build && ls | head` and `echo "cargo test | head"`,
# and is bypassed by `|&` / a newline / a path-prefixed `head` — we split the
# command into pipeline SEGMENTS on the logical separators `;`, `&&`, `||`, then
# block only a segment that is BOTH a cargo gate invocation AND pipes into
# head/tail. This scopes the pipe to its own pipeline and anchors `cargo` to the
# segment start (after optional `VAR=val` env prefixes), so a quoted literal or a
# sibling command no longer trips it. grep is intentionally NOT blocked (it does
# not truncate). Residual, accepted misses: a subshell-wrapped `( cargo test |
# head )` and an indirected pager like `| stdbuf -oL head`.

# Normalize newlines/tabs to spaces (a newline-separated pipe would dodge a
# line-oriented grep), then turn logical separators into segment boundaries.
norm="${cmd//$'\n'/ }"
norm="${norm//$'\t'/ }"
norm="${norm//';'/$'\n'}"
norm="${norm//'&&'/$'\n'}"
norm="${norm//'||'/$'\n'}"

GATE_RE='^[[:space:]]*([A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*[[:space:]]+)*cargo[[:space:]]+(\+[^[:space:]]+[[:space:]]+)?(test|clippy|build|doc|nextest)\b'
# A pipe (`|` or `|&`) into head/tail, allowing a leading path (`/usr/bin/head`).
PIPE_RE='\|&?[[:space:]]*([^[:space:]|]*/)?(head|tail)\b'

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
