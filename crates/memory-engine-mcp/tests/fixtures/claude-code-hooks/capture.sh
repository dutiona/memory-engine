#!/bin/sh
# capture.sh: capture real Claude Code hook payloads for the lifecycle endpoint's fixtures. Desktop only, never
# run by `cargo test`.
#
#   ME_TEST_CLAUDE_TOKEN_FILE=<file> sh capture.sh OUT
#
# Builds nothing: run `cargo build -p memory-engine-mcp --bin memory-engine-mcp` first. The plugin of this checkout
# queues every hook into its outbox, against plugins/memory-engine/tests/stub_server.py answering 503, so each event
# stays queued as body + headers. A scratch memory-engine-mcp on loopback serves the `recall` call that yields the
# ME MCP fixture. Every `claude` run gets a scratch HOME and an environment built from nothing; the OAuth token
# reaches it through that environment only. OUT receives the raw outbox copies; normalize.py turns them into
# fixtures.
set -u
HERE=$(cd "$(dirname "$0")" && pwd -P)
REPO=$(cd "$HERE/../../../../.." && pwd -P)
OUT=${1:?usage: capture.sh OUT}
TOKFILE=${ME_TEST_CLAUDE_TOKEN_FILE:-$HOME/.config/memory-engine/test-secrets/claude-oauth-token}
SERVER=$REPO/target/debug/memory-engine-mcp
[ -r "$TOKFILE" ] || { printf 'missing prerequisite: a readable ME_TEST_CLAUDE_TOKEN_FILE\n' >&2; exit 2; }
[ -x "$SERVER" ] || { printf 'missing prerequisite: %s\n' "$SERVER" >&2; exit 2; }
for tool in claude python3 git timeout sha256sum; do
	command -v "$tool" >/dev/null 2>&1 || { printf 'missing prerequisite: %s\n' "$tool" >&2; exit 2; }
done

R=$(cd "$(mktemp -d /tmp/me-capture.XXXXXX)" && pwd -P) || exit 2
cleanup() {
	mkdir -p "$OUT/logs" && cp "$R"/*.err "$R"/*.log "$R"/*.out "$R"/run1.json "$OUT/logs/" 2>/dev/null
	for f in "$R"/stub/pid "$R"/server.pid; do
		[ -f "$f" ] && kill "$(cat "$f")" 2>/dev/null
	done
	rm -rf "$R"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
mkdir -p "$R/bin" "$R/stub" "$R/store" "$OUT"
ln -s "$(command -v claude)" "$R/bin/claude"
H=$R/home
O=$H/.local/state/memory-engine/outbox
mkdir -p "$H/.config/memory-engine/secrets" "$H/dev/proj"
printf 'me_00000000000000000000000000000000\n' >"$H/.config/memory-engine/secrets/token"
chmod 600 "$H/.config/memory-engine/secrets/token"
printf '%s' "$R" >"$OUT/scratch-root"

# The lifecycle stub, at 503: nothing leaves the outbox.
printf 'me_00000000000000000000000000000000' >"$R/stub/token"
printf '503' >"$R/stub/status"
python3 -I "$REPO/plugins/memory-engine/tests/stub_server.py" "$R/stub" </dev/null >/dev/null 2>&1 &
printf '%s' $! >"$R/stub/pid"

# The scratch ME server for the recall call, machine laptop, with ADR-0020's test-shaped token.
MTOK=me_00000000000000000000000000laptop
printf 'laptop %s\n' "$(printf '%s' "$MTOK" | sha256sum | cut -d' ' -f1)" >"$R/tokens"
printf "[engine]\ndb_path = '%s'\nembed_dim = 4\n" "$R/store/memory.db" >"$R/config.toml"
MPORT=$(python3 -I -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
env -i PATH=/usr/bin:/bin MEMORY_MCP_USER=alice "$SERVER" --listen "127.0.0.1:$MPORT" --token-file "$R/tokens" \
	--config "$R/config.toml" </dev/null >"$R/server.log" 2>&1 &
printf '%s' $! >"$R/server.pid"
printf '{"mcpServers":{"me":{"type":"http","url":"http://127.0.0.1:%s/mcp","headers":{"Authorization":"Bearer %s"}}}}\n' \
	"$MPORT" "$MTOK" >"$R/mcp.json"

_i=0
while [ ! -s "$R/stub/port" ]; do
	_i=$((_i + 1)); [ "$_i" -lt 50 ] || exit 1; sleep 0.1
done
PORT=$(cat "$R/stub/port")

LAUNCH='
import os, sys
with open(sys.argv[1]) as f:
    token = f.read().strip().rpartition("=")[2]
env = {"HOME": sys.argv[2], "PATH": sys.argv[3], "LANG": "C.UTF-8", "DISABLE_AUTOUPDATER": "1",
       "CLAUDE_CODE_OAUTH_TOKEN": token}
extra = os.environ.get("ME_CAPTURE_EXTRA_ENV")
if extra:
    k, _, v = extra.partition("=")
    env[k] = v
os.execve(sys.argv[4], sys.argv[4:], env)
'
run_claude() {
	(cd "$H/dev/proj" && timeout 600 python3 -I -c "$LAUNCH" "$TOKFILE" "$H" "$R/bin:/usr/bin:/bin" "$R/bin/claude" "$@")
}
settle() {
	_i=0; _seen=0
	while [ "$_seen" -lt 2 ]; do
		if [ -e "$O/lock" ]; then _seen=0; else _seen=$((_seen + 1)); fi
		_i=$((_i + 1)); [ "$_i" -lt 40 ] || break
		sleep 0.5
	done
}

(
	cd "$H/dev/proj" &&
		HOME=$H GIT_CONFIG_NOSYSTEM=1 git init -q -b main &&
		printf 'fixture notes\n' >notes.txt &&
		HOME=$H GIT_CONFIG_NOSYSTEM=1 git add notes.txt &&
		HOME=$H GIT_CONFIG_NOSYSTEM=1 git -c user.name=t -c user.email=t@example.com commit -q -m init &&
		HOME=$H GIT_CONFIG_NOSYSTEM=1 git remote add origin https://user:secret@example.com/example/proj.git
) || exit 1
run_claude plugin marketplace add "$REPO" >"$R/market.log" 2>&1 || { cat "$R/market.log" >&2; exit 1; }
run_claude plugin install memory-engine@memory-engine --config "me_url=http://127.0.0.1:$PORT" \
	--config "dev_root=$H/dev" >"$R/install.log" 2>&1 || { cat "$R/install.log" >&2; exit 1; }

P1='Do these steps in order, one tool call each. 1. Run the shell command: echo fixture. 2. Read the file notes.txt.
3. Read the file missing.txt (it does not exist; that is expected). 4. Use the Task tool to start a general-purpose
subagent whose only job is to run the shell command echo subagent and report its output. 5. Call the me server recall
tool with the query: fixture. Then reply with the single word: done'
run_claude -p --output-format json --permission-mode bypassPermissions --max-turns 12 --mcp-config "$R/mcp.json" -- "$P1" \
	>"$R/run1.json" 2>"$R/run1.err"
printf 'run1 rc=%s\n' $? >&2
settle
SID=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["session_id"])' "$R/run1.json") || exit 1
run_claude -p --resume "$SID" --max-turns 1 'Reply with the single word: again' >"$R/run2.out" 2>"$R/run2.err"
printf 'run2 rc=%s\n' $? >&2
settle
run_claude -p --resume "$SID" '/compact' >"$R/run3.out" 2>"$R/run3.err"
printf 'run3 rc=%s\n' $? >&2
settle
ME_CAPTURE_EXTRA_ENV=ANTHROPIC_BASE_URL=http://127.0.0.1:9 \
	run_claude -p --max-turns 1 'Reply with the single word: ok' >"$R/run4.out" 2>"$R/run4.err"
printf 'run4 rc=%s\n' $? >&2
settle

cp -R "$O" "$OUT/outbox"
"$R/bin/claude" --version >"$OUT/claude-version" 2>/dev/null
printf 'captured %s events into %s/outbox\n' "$(ls -d "$OUT"/outbox/[0-9]* 2>/dev/null | wc -l | tr -d ' ')" "$OUT" >&2
