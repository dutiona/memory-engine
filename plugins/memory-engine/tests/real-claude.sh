#!/bin/sh
# real-claude.sh: the plugin under a real `claude -p`, against tests/stub_server.py. Desktop only.
#
#   ME_TEST_CLAUDE_TOKEN_FILE=<file> sh tests/real-claude.sh [--marketplace <source>]
#
# The file holds a token from `claude setup-token`, bare or as a `NAME=token` line (mode 0600, outside the
# repository). Every `claude` call runs under a scratch HOME, in an environment built from nothing: the token reaches
# `claude` through that environment only, never a command line, a log or a file this script writes. --marketplace installs from <source> (for example
# owner/repo#branch) instead of this checkout.
HERE=$(cd "$(dirname "$0")" && pwd -P)
PLUGIN=${HERE%/tests}
REPO=${PLUGIN%/plugins/memory-engine}
MARKET=$REPO
[ "${1-}" = --marketplace ] && MARKET=${2:?--marketplace needs a source}

TOKFILE=${ME_TEST_CLAUDE_TOKEN_FILE:-$HOME/.config/memory-engine/test-secrets/claude-oauth-token}
[ -r "$TOKFILE" ] || {
	printf 'missing prerequisite: a readable ME_TEST_CLAUDE_TOKEN_FILE\n' >&2
	exit 2
}
for tool in claude python3 git timeout; do
	command -v "$tool" >/dev/null 2>&1 || {
		printf 'missing prerequisite: %s\n' "$tool" >&2
		exit 2
	}
done

R=$(cd "$(mktemp -d /tmp/me-real.XXXXXX)" && pwd -P) || exit 2
cleanup() {
	for f in "$R"/*/stub/pid; do
		[ -f "$f" ] && kill "$(cat "$f")" 2>/dev/null
	done
	rm -rf "$R"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
mkdir "$R/bin"
ln -s "$(command -v claude)" "$R/bin/claude"

N=0
BAD=0
check() {
	N=$((N + 1))
	_name=$1
	shift
	if "$@" >"$R/check.log" 2>&1; then
		printf 'ok %d - %s\n' "$N" "$_name"
	else
		BAD=$((BAD + 1))
		printf 'not ok %d - %s\n' "$N" "$_name"
		sed 's/^/#   /' "$R/check.log"
	fi
}

# The launcher: claude under the phase's HOME, in an environment built from nothing; the token goes from its file
# into the child's environment only.
LAUNCH='
import os, sys
with open(sys.argv[1]) as f:
    token = f.read().strip().rpartition("=")[2]
env = {"HOME": sys.argv[2], "PATH": sys.argv[3], "LANG": "C.UTF-8", "DISABLE_AUTOUPDATER": "1",
       "CLAUDE_CODE_OAUTH_TOKEN": token}
os.execve(sys.argv[4], sys.argv[4:], env)
'
run_claude() {
	python3 -I -c "$LAUNCH" "$TOKFILE" "$H" "$R/bin:/usr/bin:/bin" "$R/bin/claude" "$@"
}

# phase NAME [dev_root]: a fresh HOME with a token file, the stub at 503, the plugin installed, one `claude -p` run.
phase() {
	P=$R/$1
	H=$P/home
	O=$H/.local/state/memory-engine/outbox
	mkdir -p "$P/stub" "$H/.config/memory-engine/secrets" "$H/dev/proj"
	printf 'me_00000000000000000000000000000000\n' >"$H/.config/memory-engine/secrets/token"
	chmod 600 "$H/.config/memory-engine/secrets/token"
	printf 'me_00000000000000000000000000000000' >"$P/stub/token"
	printf '503' >"$P/stub/status"
	python3 -I "$HERE/stub_server.py" "$P/stub" </dev/null >/dev/null 2>&1 &
	printf '%s' $! >"$P/stub/pid"
	_i=0
	while [ ! -s "$P/stub/port" ]; do
		_i=$((_i + 1))
		[ "$_i" -lt 50 ] || return 1
		sleep 0.1
	done
	PORT=$(cat "$P/stub/port")
	(
		cd "$H/dev/proj" &&
			HOME=$H GIT_CONFIG_NOSYSTEM=1 git init -q -b main &&
			HOME=$H GIT_CONFIG_NOSYSTEM=1 git -c user.name=t -c user.email=t@example.com commit -q --allow-empty -m init &&
			HOME=$H GIT_CONFIG_NOSYSTEM=1 git remote add origin https://user:secret@example.com/example/proj.git
	) || return 1
	run_claude plugin marketplace add "$MARKET" >"$P/market.log" 2>&1 || return 1
	_cfg="--config me_url=http://127.0.0.1:$PORT"
	[ -n "${2-}" ] && _cfg="$_cfg --config dev_root=$2 --config kb_url=http://127.0.0.1:$PORT/kb"
	run_claude plugin install memory-engine@memory-engine $_cfg >"$P/install.log" 2>&1 || return 1
	(cd "$H/dev/proj" && timeout 300 python3 -I -c "$LAUNCH" "$TOKFILE" "$H" "$R/bin:/usr/bin:/bin" "$R/bin/claude" \
		-p --max-turns 1 'Reply with the single word: ok') >"$P/claude.out" 2>"$P/claude.err"
	CLAUDE_RC=$?
	date +%s >"$P/exited"
	_i=0
	_seen=0
	while [ "$_seen" -lt 2 ]; do
		if [ -e "$O/lock" ]; then _seen=0; else _seen=$((_seen + 1)); fi
		_i=$((_i + 1))
		[ "$_i" -lt 40 ] || break
		sleep 0.5
	done
	return 0
}

# hooks_queued DIR NAME...: each named hook_event_name has a queued body (Stop may be StopFailure).
hooks_queued() {
	python3 -I -c '
import glob, json, os, sys
names = []
for d in sorted(glob.glob(os.path.join(sys.argv[1], "[0-9]*"))):
    with open(os.path.join(d, "body"), "rb") as f:
        names.append(json.loads(f.read())["hook_event_name"])
print("queued:", " ".join(names))
for want in sys.argv[2:]:
    alts = want.split("|")
    assert any(a in names for a in alts), "no " + want
' "$@"
}

headers_have() {
	python3 -I -c '
import glob, os, sys
want = sys.argv[2:]
for d in sorted(glob.glob(os.path.join(sys.argv[1], "[0-9]*"))):
    lines = open(os.path.join(d, "headers")).read().splitlines()
    for w in want:
        if w.startswith("!"):
            assert not any(l.startswith(w[1:]) for l in lines), (d, w)
        else:
            assert w in lines, (d, w)
' "$@"
}

phase A "$R/A/home/dev" || {
	printf 'not ok - phase A setup failed\n'
	cat "$R/A/market.log" "$R/A/install.log" 2>/dev/null | sed 's/^/#   /'
	exit 1
}
check "real: claude -p exits 0" test "$CLAUDE_RC" -eq 0
check "real: SessionStart, UserPromptSubmit, Stop or StopFailure, and SessionEnd are queued after exit" \
	hooks_queued "$O" SessionStart UserPromptSubmit 'Stop|StopFailure' SessionEnd
check "real: the outbox endpoint holds the configured me_url" test "$(cat "$O/endpoint")" = "http://127.0.0.1:$PORT"

check "real: the project facts, userinfo removed" headers_have "$O" "ME-Project-Root: $R/A/home/dev/proj" \
	"ME-Project-Dev-Root: $R/A/home/dev" "ME-Project-Origin-Url: https://example.com/example/proj.git" \
	"ME-Project-Branch: main"

# delivered IDS-DIR STUB-DIR: every event of the snapshot got exactly one 204, with its queued body, and no other did.
delivered() {
	python3 -I -c '
import collections, glob, os, sys
snap, stub = sys.argv[1], sys.argv[2]
ids = {os.path.basename(d) for d in glob.glob(os.path.join(snap, "[0-9]*"))}
lines = [l.split() for l in open(os.path.join(stub, "log"))]
ok = collections.Counter(l[2] for l in lines if l[1] == "204")
assert set(ok) == ids and set(ok.values()) == {1}, (sorted(ids), ok)
for n, l in enumerate(lines, 1):
    if l[1] == "204":
        with open(os.path.join(stub, "req", "%d.body" % n), "rb") as a, open(os.path.join(snap, l[2], "body"), "rb") as b:
            assert a.read() == b.read(), l[2]
print("delivered:", len(ids))
' "$@"
}

only_503() {
	[ ! -f "$1/log" ] || ! grep -qv '^[0-9]* 503 ' "$1/log"
}

empty_outbox() {
	set -- "$O"/[0-9]* "$O"/rejected/[0-9]*
	for f; do [ ! -e "$f" ] || return 1; done
}

# mcp_wire STUB OUTBOX TOKEN DEVROOT: the MCP requests the stub recorded carry the hooks' own facts. Every /mcp request
# holds the token, the envelope, the configured dev root and exactly the ME-Project-* values of a queued event; every
# /kb/mcp request holds the token and no ME- header; no request holds userinfo.
mcp_wire() {
	python3 -I -c '
import glob, os, sys
stub, outbox, token, dev = sys.argv[1:5]
def headers(path):
    out = {}
    for line in open(path, encoding="latin-1").read().splitlines():
        if ": " in line:
            k, v = line.split(": ", 1)
            out[k.lower()] = v
    return out
paths = {}
for k, line in enumerate(open(os.path.join(stub, "other.log")), 1):
    paths.setdefault(line.split()[2], []).append(k)
events = [headers(os.path.join(d, "headers")) for d in glob.glob(os.path.join(outbox, "[0-9]*"))]
assert events, "no queued event to compare with"
facts = {k: v for k, v in events[0].items() if k.startswith("me-project-")}
me = [headers(os.path.join(stub, "other", "%d.headers" % k)) for k in paths.get("/mcp", [])]
kb = [headers(os.path.join(stub, "other", "%d.headers" % k)) for k in paths.get("/kb/mcp", [])]
assert me and kb, sorted(paths)
for h in me:
    assert h.get("authorization") == "Bearer " + token and h.get("me-harness") == "claude-code", sorted(h)
    assert h.get("me-client-version"), sorted(h)
    got = {k: v for k, v in h.items() if k.startswith("me-project-")}
    assert got == facts and got.get("me-project-dev-root") == dev, (got, facts)
for h in kb:
    assert h.get("authorization") == "Bearer " + token, sorted(h)
    assert not [k for k in h if k.startswith("me-")], sorted(h)
for f in glob.glob(os.path.join(stub, "other", "*")):
    assert b"secret" not in open(f, "rb").read(), f
print("mcp requests:", len(me), "me,", len(kb), "kb")
' "$@"
}

check "real: the stub answered only 503 during the run" only_503 "$R/A/stub"
check "real: the MCP requests carry the token and the hooks' facts; KB's only the token" \
	mcp_wire "$R/A/stub" "$O" me_00000000000000000000000000000000 "$R/A/home/dev"
if [ -f "$R/A/stub/log" ]; then
	_late=$(awk -v t="$(cat "$R/A/exited")" '$1 >= t * 1000' "$R/A/stub/log" | wc -l | tr -d ' ')
	printf '# info: %s of %s requests reached the stub after claude -p exited\n' "$_late" "$(wc -l <"$R/A/stub/log" | tr -d ' ')"
fi
cp -R "$O" "$R/A/snapshot"
printf 204 >"$R/A/stub/status"
env -i /bin/sh "$H/.local/state/memory-engine/bin/me-hook" flush
check "real: each queued event delivered exactly once, SessionEnd included" delivered "$R/A/snapshot" "$R/A/stub"
check "real: the outbox and rejected/ are empty" empty_outbox
_before=$(wc -l <"$R/A/stub/log")
env -i /bin/sh "$H/.local/state/memory-engine/bin/me-hook" flush
check "real: a second flush sends nothing" test "$(wc -l <"$R/A/stub/log")" -eq "$_before"
if [ "$MARKET" != "$REPO" ]; then
	set -- "$H"/.claude/plugins/cache/memory-engine/memory-engine/*/bin/me-hook
	check "real: the stable copy is the installed plugin's helper" test $# -eq 1 -a -f "$1"
	check "real: the stable copy equals the cached helper" cmp "$1" "$H/.local/state/memory-engine/bin/me-hook"
fi

phase U || {
	printf 'not ok - phase U setup failed\n'
	exit 1
}
check "real: hooks run and queue with an option unset" hooks_queued "$O" SessionStart SessionEnd
check "real: no dev root is sent when dev_root is unset" headers_have "$O" "!ME-Project-Dev-Root"

printf '# %d checks, %d not ok\n' "$N" "$BAD"
[ "$BAD" -eq 0 ]
