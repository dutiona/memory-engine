#!/bin/sh
# real-claude.sh: the plugin under a real `claude -p`, against tests/stub_server.py. Desktop only.
#
#   ME_TEST_CLAUDE_TOKEN_FILE=<file> ME_TEST_KB_DIR=<dir> [ME_TEST_ME_BIN=<file>] sh tests/real-claude.sh [--marketplace <source>]
#
# Phases A and U run against tests/stub_server.py. Phases M and L (#1028) run the plugin's MCP servers against a local
# memory-engine-mcp (ME_TEST_ME_BIN, default target/debug/memory-engine-mcp of this checkout, with memory-engine-cli
# beside it) and a local knowledge-base (ME_TEST_KB_DIR, a knowledge-base checkout whose .venv was synced with
# `uv sync --locked`), on loopback ports, through tests/real_mcp.py. Nothing here builds, syncs or downloads.
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
ME_BIN=${ME_TEST_ME_BIN:-$REPO/target/debug/memory-engine-mcp}
CLI_BIN=${ME_BIN%/*}/memory-engine-cli
KB_BIN=${ME_TEST_KB_DIR:+$ME_TEST_KB_DIR/.venv/bin/knowledge-base}
for f in "$ME_BIN" "$CLI_BIN" "${KB_BIN:-ME_TEST_KB_DIR}"; do
	[ -x "$f" ] || {
		printf 'missing prerequisite: %s\n' "$f" >&2
		exit 2
	}
done
for tool in claude python3 git timeout curl; do
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

# ---------------------------------------------------------------- phases M and L: the MCP servers (#1028)

# free_port: a loopback port nothing listens on now; kept for the phase, because the plugin's URLs are fixed at install.
free_port() {
	python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'
}

# mcp_phase NAME physical|link yes|no: a scratch HOME whose dev root dev-real is also reached through the symlink dev,
# the remote-less project "café 'x" and the repository other under it, and the plugin installed against a local ME,
# and a local KB when the third argument is yes. dev_root is the README's `cd ~/dev && pwd -P` (physical), or the
# symlink itself (link). Sets P H O PROJ Q MP KP DEVROOT.
mcp_phase() {
	P=$R/$1
	H=$P/home
	O=$H/.local/state/memory-engine/outbox
	PROJ="$H/dev-real/café 'x"
	Q=$H/dev-real/other
	mkdir -p "$PROJ" "$Q" "$H/.config/memory-engine/secrets" || return 1
	ln -s "$H/dev-real" "$H/dev" || return 1
	(
		HOME=$H GIT_CONFIG_NOSYSTEM=1
		export HOME GIT_CONFIG_NOSYSTEM
		for d in "$PROJ" "$Q"; do
			git init -q -b main "$d" &&
				git -C "$d" -c user.name=t -c user.email=t@example.com commit -q --allow-empty -m init || exit 1
		done
		git -C "$Q" remote add origin https://example.com/example/other.git
	) || return 1
	DEVROOT=$H/dev
	[ "$2" = physical ] && DEVROOT=$(cd "$H/dev" && pwd -P)
	MP=$(free_port) || return 1
	KP=
	if [ "$3" = yes ]; then KP=$(free_port) || return 1; fi
	run_claude plugin marketplace add "$MARKET" >"$P/market.log" 2>&1 || return 1
	_cfg="--config me_url=http://127.0.0.1:$MP --config dev_root=$DEVROOT"
	[ -n "$KP" ] && _cfg="$_cfg --config kb_url=http://127.0.0.1:$KP"
	run_claude plugin install memory-engine@memory-engine $_cfg >"$P/install.log" 2>&1
}

# mcp_run TURNS RESUME KB-QUERIES SERVERS (JSON each): real_mcp.py over this phase; SERVERS maps each plugin server to
# the status its init event must show.
mcp_run() {
	python3 -I -c '
import json, sys
keys = ["dir", "home", "project", "pwd", "claude", "claude_token_file", "path", "me_bin", "cli_bin", "kb_bin",
        "stub_embedder", "me_port", "kb_port", "machine", "add_dir", "dev_root", "nonce"]
cfg = dict(zip(keys, sys.argv[1:18]))
cfg["me_port"] = int(cfg["me_port"])
cfg["kb_port"] = int(cfg["kb_port"]) if cfg["kb_port"] else None
cfg["add_dirs"] = [cfg.pop("add_dir")]
cfg["turns"], cfg["resume"], cfg["kb_queries"], cfg["want_servers"] = (json.loads(a) for a in sys.argv[18:22])
tools = ["mcp__plugin_memory-engine_me__" + t for t in ("recall", "remember", "forget", "set_level")]
tools += ["mcp__plugin_memory-engine_kb__" + t for t in ("search_index", "ingest_text", "ingest_url")]
cfg["allowed_tools"] = tools + ["Bash(cd:*)", "Bash(pwd)"]
json.dump(cfg, open(cfg["dir"] + "/config.json", "w"), ensure_ascii=False, indent=1)
' "$P" "$H" "$PROJ" "$H/dev/café 'x" "$R/bin/claude" "$TOKFILE" "$R/bin:/usr/bin:/bin" "$ME_BIN" "$CLI_BIN" \
		"${KB_BIN-}" "$HERE/stub_embedder.py" "$MP" "$KP" realrun "$Q" "$DEVROOT" "$NONCE" "$1" "$2" "$3" "$4" || return 1
	timeout 1800 python3 -I "$HERE/real_mcp.py" "$P/config.json" >"$P/driver.log" 2>&1
}

# turns NONCE OTHER: the scripted turns of phase M, each naming its exact tool calls (real_mcp.py checks that the model
# made them before anything else is judged).
turns() {
	python3 -I -c '
import json, sys
n, q = sys.argv[1:3]
me, kb = "mcp__plugin_memory-engine_me__", "mcp__plugin_memory-engine_kb__"
only = "Call nothing else, then reply with the single word: done"
def t(label, prompt, expect, after=None):
    return {"label": label, "prompt": prompt + " " + only, "expect": expect, "after": after}
recall = "Call %srecall with query \"alpha-%s\". Then call %ssearch_index with query \"bravo-%s\"." % (me, n, kb, n)
both = [[me + "recall", "alpha-" + n], [kb + "search_index", "bravo-" + n]]
print(json.dumps([
    t("T1", "Call %sremember with content \"alpha-%s is the codename of this test\". Then call %singest_text with "
      "name \"note-%s\" and content \"bravo-%s is a document about this test\"." % (me, n, kb, n, n),
      [[me + "remember", "alpha-" + n], [kb + "ingest_text", "bravo-" + n]], "restart"),
    t("T2", recall, both, "rotate"),
    t("T3", recall, both),
    t("T4", "Run the Bash command: cd %s && pwd. Then call %sremember with content \"charlie-%s is the second fact "
      "of this test\"." % (json.dumps(q), me, n), [["Bash", "other"], [me + "remember", "charlie-" + n]]),
]))
print(json.dumps(t("R1", "Call %srecall with query \"charlie-%s\"." % (me, n), [[me + "recall", "charlie-" + n]])))
' "$@"
}

NONCE=$(python3 -I -c 'import secrets; print(secrets.token_hex(4))')
mcheck() {
	_name=$1
	shift
	check "$_name" python3 -I "$HERE/real_mcp.py" "$P/config.json" "$@"
}

mcp_phase M physical yes || {
	printf 'not ok - phase M setup failed\n'
	cat "$R/M/market.log" "$R/M/install.log" 2>/dev/null | sed 's/^/#   /'
	exit 1
}
turns "$NONCE" "$Q" >"$P/turns.txt"
mcp_run "$(sed -n 1p "$P/turns.txt")" "$(sed -n 2p "$P/turns.txt")" "[\"bravo-$NONCE\"]" \
	'{"plugin:memory-engine:me": "connected", "plugin:memory-engine:kb": "connected"}'
if [ -e "$P/out/precondition" ] || [ ! -s "$P/out/export.json" ]; then
	printf 'not ok - phase M did not complete: %s\n' "$(cat "$P/out/precondition" 2>/dev/null || tail -n 3 "$P/driver.log")"
	exit 1
fi
mcheck "real: ME and KB refuse a request without a token, and a rotated-out token, and take the current one" auth
mcheck "real: the init events list exactly the me and kb servers, connected, and the seven tools" tools
mcheck "real: every scripted MCP call answered without error, each ME answer opening with its call line" calls
mcheck "real: every hook event and every MCP call record has the hooks' key, physical root and dev root" key
mcheck "real: after restarting ME and KB, recall and search_index find what the session filed" restart
mcheck "real: every ME call record is linked to its session; every KB call is captured in it" session
mcheck "real: after a token rotation the same session keeps working (the helper read the new token)" rotation
mcheck "real: after a cd into another repository, remember still files under the session's project" cd
mcheck "real: a --resume of the session recalls what it filed, under the same session" resume
check "real: the outbox and rejected/ are empty after the flush" empty_outbox
mcheck "real: no machine token or Claude token in any file the phase wrote" leaks
python3 -I "$HERE/real_mcp.py" "$P/config.json" cwd-info

# Phase L: dev_root configured through the symlink, and no kb_url. One outcome on both channels: no key at all.
mcp_phase L link no || {
	printf 'not ok - phase L setup failed\n'
	exit 1
}
mcp_run "[$(python3 -I -c '
import json, sys
print(json.dumps({"label": "L1", "prompt": "Call mcp__plugin_memory-engine_me__remember with content \"delta-%s is a fact\". Call nothing else, then reply with the single word: done" % sys.argv[1],
                  "expect": [["mcp__plugin_memory-engine_me__remember", "delta-" + sys.argv[1]]], "after": None}))' "$NONCE")]" null '[]' \
	'{"plugin:memory-engine:me": "connected", "plugin:memory-engine:kb": "failed"}'
[ -s "$P/out/export.json" ] || {
	printf 'not ok - phase L did not complete: %s\n' "$(tail -n 3 "$P/driver.log")"
	exit 1
}
mcheck "real: without kb_url, only the kb server fails" tools
mcheck "real: a dev root configured through a symlink gives no key on either channel, never two" unscoped
printf '# %d checks, %d not ok\n' "$N" "$BAD"
[ "$BAD" -eq 0 ]
