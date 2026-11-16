#!/bin/sh
# Shell tests for bin/me-hook, the ADR-0020 client. POSIX sh: `sh tests/run.sh [case ...]`, under dash and under
# macOS /bin/sh (bash 3.2). Every case runs under a scratch HOME; the helper's process tree runs on a PATH holding
# only the utilities it may use, so any other command fails the case.
HERE=$(cd "$(dirname "$0")" && pwd -P)
PLUGIN=${HERE%/tests}
REPO=${PLUGIN%/plugins/memory-engine}

for tool in python3 git pgrep curl; do
	command -v "$tool" >/dev/null 2>&1 || {
		printf 'missing prerequisite: %s\n' "$tool" >&2
		exit 2
	}
done

GIT_BIN=$(command -v git)
case $(uname) in
Darwin)
	GIT_BIN=$(xcrun -f git 2>/dev/null || command -v git)
	CASE_LOCALE=en_US.UTF-8
	;;
*) CASE_LOCALE=C.UTF-8 ;;
esac

T=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/me hook.XXXXXX")" && pwd -P) || exit 2
cleanup() {
	for f in "$T"/*/stub.pid; do
		[ -f "$f" ] && kill "$(cat "$f")" 2>/dev/null
	done
	pkill -f "$T/" 2>/dev/null
	rm -rf "$T"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

TOKV=me_00000000000000000000000000000000
NL='
'

fail() {
	printf '%s\n' "$*"
	exit 1
}

# mkrbin DIR fake|real|none: the helper's whole PATH.
mkrbin() {
	mkdir -p "$1"
	for u in sh mkdir mv rm cat date; do
		ln -s "$(command -v "$u")" "$1/$u"
	done
	ln -s "$GIT_BIN" "$1/git"
	case $2 in
	fake) ln -s "$HERE/fake-curl" "$1/curl" ;;
	real) ln -s "$(command -v curl)" "$1/curl" ;;
	esac
}

# wrap NAME BODY: replace one utility of the case's PATH with a script; BODY runs before the real one and sees "$@".
wrap() {
	_real=$(command -v "$1")
	rm -f "$RB/$1"
	printf '#!/bin/sh\n%s\nexec %s "$@"\n' "$2" "$_real" >"$RB/$1"
	chmod 755 "$RB/$1"
}

setup_case() {
	HOME=$C/home
	export HOME
	unset XDG_CONFIG_HOME XDG_STATE_HOME GIT_DIR GIT_WORK_TREE CLAUDE_PROJECT_DIR http_proxy https_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY all_proxy
	GIT_CONFIG_NOSYSTEM=1
	GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.com GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.com
	export GIT_CONFIG_NOSYSTEM GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL
	LC_ALL=$CASE_LOCALE
	export LC_ALL
	S=$HOME/.local/state/memory-engine
	O=$S/outbox
	TOKF=$HOME/.config/memory-engine/secrets/token
	mkdir -p "$C/plugin" "$C/curl.d"
	cp -R "$PLUGIN/bin" "$C/plugin/"
	HB=$C/plugin/bin/me-hook
	RB=$C/rbin
	mkrbin "$RB" fake
	FAKE_CURL_DIR=$C/curl.d
	export FAKE_CURL_DIR
	URL=http://127.0.0.1:9
	DEV=
	printf '{"hook_event_name":"PostToolUse"}' >"$C/in"
}

# hook SUB [URL [DEV]] < input: run the helper as a hook does, on the restricted PATH.
hook() {
	_sub=$1
	PATH=$RB /bin/sh "$HB" "$_sub" "${2-$URL}" "${3-$DEV}" >"$C/out" 2>"$C/err"
	_rc=$?
	[ "$_rc" -eq 0 ] || fail "hook $_sub exited $_rc"
	if [ "$_sub" = capture ]; then
		[ ! -s "$C/out" ] || fail "capture printed on stdout: $(cat "$C/out")"
	fi
}

# flush_fg: one flush in the foreground, as the watcher would run it from a hook's environment.
flush_fg() {
	PATH=$RB /bin/sh "$HB" flush </dev/null >"$C/fout" 2>&1
}

settle() {
	_i=0
	while pgrep -f "$C/plugin/bin/me-hook" >/dev/null 2>&1; do
		_i=$((_i + 1))
		[ "$_i" -lt 100 ] || fail "flushers still running after 10 s"
		sleep 0.1
	done
}

token() {
	mkdir -p "${TOKF%/*}"
	printf '%s\n' "$TOKV" >"$TOKF"
}

# events [DIR]: the queued event directories, in glob (order-key) order, one name per line.
events() {
	for _e in "${1-$O}"/[0-9]*; do
		[ -d "$_e" ] && printf '%s\n' "${_e##*/}"
	done
}

count() {
	_n=0
	for _e in "${1-$O}"/[0-9]*; do
		[ -d "$_e" ] && _n=$((_n + 1))
	done
	printf '%s\n' "$_n"
}

eq() {
	[ "$1" = "$2" ] || fail "${3:-mismatch}: got [$1], want [$2]"
}

py() {
	_code=$1
	shift
	python3 -I -c "$_code" "$@"
}

# header EVENTDIR NAME: the value of one header line, or nothing.
header() {
	while IFS= read -r _l; do
		case $_l in "$2: "*) printf '%s\n' "${_l#"$2: "}" ;; esac
	done <"$1/headers"
}

# ---------------------------------------------------------------- item 1: manifest and registration

t_registration() {
	py '
import json, sys
d = json.load(open(sys.argv[1]))
events = ["SessionStart", "UserPromptSubmit", "SubagentStart", "PostToolUse", "PostToolUseFailure",
          "Stop", "StopFailure", "SubagentStop", "PostCompact", "SessionEnd"]
hooks = d["hooks"]
assert sorted(hooks) == sorted(events), sorted(hooks)
for e in events:
    groups = hooks[e]
    assert len(groups) == 1 and set(groups[0]) == {"hooks"}, (e, groups)
    sub = "session-start" if e == "SessionStart" else "capture"
    want = {"type": "command", "command": "/bin/sh",
            "args": ["${CLAUDE_PLUGIN_ROOT}/bin/me-hook", sub, "${user_config.me_url}", "${user_config.dev_root}"],
            "timeout": 5}
    assert groups[0]["hooks"] == [want], (e, groups[0]["hooks"])
' "$PLUGIN/hooks/hooks.json" || fail "hooks.json is not the ten ADR-0020 handlers"
	for never in PreCompact PreModelSwitch WorktreeCreate WorktreeRemove MessageDisplay async; do
		! grep -q "$never" "$PLUGIN/hooks/hooks.json" || fail "hooks.json names $never"
	done
}

t_version() {
	v=$(sed -n 's/^ME_VERSION=//p' "$PLUGIN/bin/me-hook")
	py '
import json, sys
p = json.load(open(sys.argv[1]))
m = json.load(open(sys.argv[2]))
assert p["version"] == sys.argv[3], (p["version"], sys.argv[3])
assert [x["source"] for x in m["plugins"] if x["name"] == "memory-engine"] == ["./plugins/memory-engine"]
assert set(p["userConfig"]) == {"me_url", "dev_root"}
assert not any(o.get("sensitive") or o.get("required") for o in p["userConfig"].values())
' "$PLUGIN/.claude-plugin/plugin.json" "$REPO/.claude-plugin/marketplace.json" "$v" ||
		fail "plugin.json, marketplace.json and ME_VERSION disagree"
}

# ---------------------------------------------------------------- runner

CASES=${CASES:-"
registration version
"}

N=0
OK=0
BAD=0
[ $# -gt 0 ] && CASES=$*
for name in $CASES; do
	N=$((N + 1))
	C=$T/$name
	mkdir -p "$C/home"
	if (cd "$C" && setup_case && "t_$name") >"$C/log" 2>&1; then
		OK=$((OK + 1))
		printf 'ok %d - %s\n' "$N" "$name"
	else
		BAD=$((BAD + 1))
		printf 'not ok %d - %s\n' "$N" "$name"
		sed 's/^/#   /' "$C/log" | tail -n 20
	fi
	[ -f "$C/stub.pid" ] && kill "$(cat "$C/stub.pid")" 2>/dev/null
	pkill -f "$C/plugin/bin/me-hook" 2>/dev/null
done
printf '# %d ok, %d not ok\n' "$OK" "$BAD"
[ "$BAD" -eq 0 ]
