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
	for f in "$T"/*/stub*.pid; do
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

# ---------------------------------------------------------------- item 2: enqueue

# hook_as_pid SCRIPT SUB: run SCRIPT in a shell that then execs the helper, so SCRIPT knows the hook's pid as $$.
hook_as_pid() {
	PATH=$RB /bin/sh -c "$1"'
exec /bin/sh "$0" "$@"' "$HB" "$2" "$URL" "$DEV" <"$C/in" >"$C/out" 2>"$C/err" || fail "hook exited $?"
}

t_body_identical() {
	py 'import sys; sys.stdout.buffer.write(bytes(range(256)))' >"$C/in"
	hook capture <"$C/in"
	printf 'a\r\nb\r\n' >"$C/in2"
	hook capture <"$C/in2"
	py 'import os, sys; sys.stdout.buffer.write(os.urandom(1 << 20))' >"$C/in3"
	hook capture <"$C/in3"
	set -- $(events)
	[ $# -eq 3 ] || fail "want 3 events, got $#"
	cmp "$C/in" "$O/$1/body" && cmp "$C/in2" "$O/$2/body" && cmp "$C/in3" "$O/$3/body" || fail "a body differs from its input"
}

t_envelope() {
	hook capture <"$C/in"
	id=$(events)
	py '
import re, sys, time
id, path = sys.argv[1], sys.argv[2]
m = re.fullmatch(r"([0-9]{10})-([0-9]{6})-([0-9]+)", id)
assert m, id
lines = open(path).read().split("\n")
assert lines[-1] == "", "headers must end with a line feed"
h = dict(l.split(": ", 1) for l in lines[:-1])
assert list(h) == ["ME-Event-Id", "ME-Event-Time", "ME-Event-Seq", "ME-Harness", "ME-Client-Version"], list(h)
assert h["ME-Event-Id"] == id
assert re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}", id)
assert h["ME-Event-Time"] == time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(int(m.group(1)))), h
assert h["ME-Event-Seq"] == m.group(2)
assert h["ME-Harness"] == "claude-code" and h["ME-Client-Version"] == "0.1.0", h
' "$id" "$O/$id/headers" || fail "envelope off ADR-0020's grammar"
}

t_order() {
	for i in 1 2 3 4 5; do
		printf 'event %s' "$i" >"$C/in"
		hook capture <"$C/in"
	done
	i=0
	for id in $(events); do
		i=$((i + 1))
		eq "$(cat "$O/$id/body")" "event $i" "glob order"
	done
	eq "$i" 5 "event count"
}

t_seq_taken() {
	e=$(date +%s)
	mkdir -p "$O/seq" "$O/tmp" "$O/rejected"
	for d in 0 1 2 3; do mkdir "$O/seq/$((e + d)).0"; done
	hook capture <"$C/in"
	id=$(events)
	case $id in *-000001-*) ;; *) fail "want seq 000001, got $id" ;; esac
}

t_seq_bound() {
	e=$(date +%s)
	mkdir -p "$O/seq" "$O/tmp" "$O/rejected"
	py '
import os, sys
o, e = sys.argv[1], int(sys.argv[2])
for d in range(4):
    for n in range(1000):
        os.mkdir("%s/seq/%d.%d" % (o, e + d, n))
' "$O" "$e"
	hook capture <"$C/in"
	eq "$(count)" 0 "events queued with every seq taken"
	rm -rf "$O/seq"
	mkdir "$O/seq"
	chmod 500 "$O/seq"
	hook capture <"$C/in"
	chmod 700 "$O/seq"
	eq "$(count)" 0 "events queued with a read-only seq/"
	set -- "$O"/tmp/*
	[ ! -e "$1" ] || fail "tmp/ not empty: $*"
}

t_pid_reuse() {
	hook_as_pid 'mkdir -p "$HOME/.local/state/memory-engine/outbox/tmp/$$/stale"' capture
	eq "$(count)" 1 "events"
	[ ! -e "$O/$(events)/stale" ] || fail "the leftover tmp dir was published"
}

t_id_collision() {
	hook_as_pid 'o=$HOME/.local/state/memory-engine/outbox; e=$(date +%s); mkdir -p "$o/tmp" "$o/seq" "$o/rejected"
for d in 0 1 2 3; do mkdir -p "$o/$((e + d))-000000-$$"; printf old >"$o/$((e + d))-000000-$$/body"; done' capture
	n=0
	for id in $(events); do
		case $id in
		*-000000-*) eq "$(cat "$O/$id/body")" old "an existing event changed" ;;
		*-000001-*) n=$((n + 1)) ;;
		esac
		set -- "$O/$id"/[0-9]*
		[ ! -e "$1" ] || fail "an event was nested inside $id"
	done
	eq "$n" 1 "the new event under the next seq"
}

t_rejected_collision() {
	hook_as_pid 'o=$HOME/.local/state/memory-engine/outbox; e=$(date +%s); mkdir -p "$o/tmp" "$o/seq" "$o/rejected"
for d in 0 1 2 3; do mkdir -p "$o/rejected/$((e + d))-000000-$$"; done' capture
	case $(events) in *-000001-*) ;; *) fail "want the next seq, got $(events)" ;; esac
}

t_tree_heals() {
	hook capture <"$C/in"
	for d in "$O/tmp" "$O/seq"; do
		rm -rf "$d"
		hook capture <"$C/in"
		[ -d "$d" ] || fail "$d was not recreated"
	done
	eq "$(count)" 3 "events"
}

t_body_write_fail() {
	wrap cat '[ $# -eq 0 ] && { dd bs=1 count=3 2>/dev/null; exit 1; }'
	hook capture <"$C/in"
	eq "$(count)" 0 "events"
	set -- "$O"/tmp/*
	[ ! -e "$1" ] || fail "tmp/ not empty: $*"
}

t_endpoint() {
	hook capture http://127.0.0.1:9/ <"$C/in"
	eq "$(cat "$O/endpoint")" http://127.0.0.1:9 "recorded endpoint"
	hook capture https://example.com/me <"$C/in"
	eq "$(cat "$O/endpoint")" https://example.com/me "changed endpoint"
	for bad in '' '${user_config.me_url}' ftp://x 'http://a b'; do
		hook capture "$bad" <"$C/in"
		eq "$(cat "$O/endpoint")" https://example.com/me "endpoint after [$bad]"
	done
	eq "$(count)" 6 "every event queued"
}

t_side_duties_fail() {
	hook capture http://127.0.0.1:9 <"$C/in"
	wrap mv 'case $1 in *.endpoint) exit 1 ;; esac'
	hook capture https://example.com/new <"$C/in"
	[ ! -e "$O/endpoint" ] || fail "a failed endpoint update left $(cat "$O/endpoint")"
	eq "$(count)" 2 "events"
}

t_silent() {
	env -u HOME PATH="$RB" /bin/sh "$HB" capture "$URL" "" <"$C/in" >"$C/out" 2>&1 || fail "no HOME: exit $?"
	[ ! -s "$C/out" ] || fail "no HOME: printed $(cat "$C/out")"
	mkdir -p "$HOME/.local"
	: >"$HOME/.local/state"
	hook capture <"$C/in"
	rm "$HOME/.local/state"
	hook capture </dev/null
	eq "$(count)" 1 "events"
	[ ! -s "$O/$(events)/body" ] || fail "empty stdin gave a body"
}

t_modes() {
	hook capture <"$C/in"
	hook session-start <"$C/in"
	out=$(find "$S" \( -type d ! -perm 700 \) -o \( -type f ! -perm 600 \))
	[ -z "$out" ] || fail "wrong modes: $out"
}

# ---------------------------------------------------------------- item 4: project facts

mkrepo() {
	git init -q -b main "$1" && git -C "$1" commit -q --allow-empty -m init
}

last() {
	events | tail -n 1
}

# facts_of [DIR]: capture with CLAUDE_PROJECT_DIR=DIR and print the event's ME-Project-* lines.
facts_of() {
	CLAUDE_PROJECT_DIR=$1
	export CLAUDE_PROJECT_DIR
	hook capture <"$C/in"
	_l=$(last)
	while IFS= read -r _h; do
		case $_h in ME-Project-*) printf '%s\n' "$_h" ;; esac
	done <"$O/$_l/headers"
}

# pct_oracle VALUE: ADR-0020 §3's encoding, computed independently.
pct_oracle() {
	py '
import os, sys
v = os.fsencode(sys.argv[1])
print("".join(chr(b) if 0x21 <= b <= 0x7E and chr(b) not in "%\"\\" else "%%%02X" % b for b in v))
' "$1"
}

t_facts_repo() {
	mkrepo "$C/r"
	git -C "$C/r" checkout -q -b feat/x
	git -C "$C/r" remote add upstream git@example.org:up/proj.git
	git -C "$C/r" remote add origin https://ci-user:not-a-secret@example.com/org/proj.git
	DEV=/x/dev
	got=$(facts_of "$C/r")
	want="ME-Project-Root: $(pct_oracle "$C/r")
ME-Project-Dev-Root: /x/dev
ME-Project-Upstream-Url: example.org:up/proj.git
ME-Project-Origin-Url: https://example.com/org/proj.git
ME-Project-Branch: feat/x"
	eq "$got" "$want" "facts"
}

t_facts_worktree() {
	mkrepo "$C/r"
	git -C "$C/r" worktree add -q -b wt "$C/w"
	eq "$(facts_of "$C/w")" "ME-Project-Root: $(pct_oracle "$C/r")
ME-Project-Branch: wt" "linked worktree facts"
}

t_facts_subdir() {
	mkrepo "$C/r"
	mkdir -p "$C/r/a/b"
	eq "$(facts_of "$C/r/a/b")" "ME-Project-Root: $(pct_oracle "$C/r")
ME-Project-Branch: main" "subdirectory facts"
}

t_facts_detached() {
	mkrepo "$C/r"
	git -C "$C/r" checkout -q --detach
	eq "$(facts_of "$C/r")" "ME-Project-Root: $(pct_oracle "$C/r")" "detached facts"
}

t_facts_non_git() {
	mkdir "$C/plain"
	DEV=/x/dev
	eq "$(facts_of "$C/plain")" "ME-Project-Root: $(pct_oracle "$C/plain")
ME-Project-Dev-Root: /x/dev" "non-git facts"
}

t_facts_cwd() {
	mkrepo "$C/r"
	git -C "$C/r" remote add origin https://example.com/o/r.git
	mkrepo "$C/other"
	git -C "$C/other" checkout -q -b elsewhere
	git -C "$C/other" remote add origin https://example.com/o/other.git
	ref=$(facts_of "$C/r")
	got1=$(cd / && facts_of "$C/r")
	got2=$(cd "$C/other" && facts_of "$C/r")
	got3=$(GIT_DIR=$C/other/.git && export GIT_DIR && facts_of "$C/r")
	eq "$got1" "$ref" "facts from /"
	eq "$got2" "$ref" "facts from another repository"
	eq "$got3" "$ref" "facts with GIT_DIR set"
}

t_facts_insteadof() {
	mkrepo "$C/r"
	git -C "$C/r" config url.https://example.com/.insteadOf ex:
	git -C "$C/r" remote add origin ex:org/proj
	case $(facts_of "$C/r") in *"ME-Project-Origin-Url: https://example.com/org/proj"*) ;; *) fail "insteadOf not applied" ;; esac
}

t_userinfo() {
	mkrepo "$C/r"
	git -C "$C/r" remote add origin x
	while IFS='|' read -r in want; do
		git -C "$C/r" remote set-url origin "$in"
		got=$(facts_of "$C/r" | sed -n 's/^ME-Project-Origin-Url: //p')
		eq "$got" "$want" "userinfo of $in"
	done <<'EOF'
https://user:secret@example.com/o/r.git|https://example.com/o/r.git
https://tok123@example.com/o/r|https://example.com/o/r
https://a@b@example.com/o/r|https://example.com/o/r
ssh://git@example.com:22/o/r.git|ssh://example.com:22/o/r.git
git@example.com:o/r.git|example.com:o/r.git
/srv/git/r.git|/srv/git/r.git
https://example.com/o/r@v2|https://example.com/o/r@v2
EOF
	! grep -rq -e secret -e tok123 "$S" || fail "userinfo reached the state root"
}

t_encoding() {
	py '
import os, sys
os.makedirs(os.path.join(sys.argv[1], "enc", "x \x27\"%\\é日"))
' "$C"
	for d in "$C"/enc/*; do :; done
	got=$(facts_of "$d" | sed -n 's/^ME-Project-Root: //p')
	eq "$got" "$(pct_oracle "$d")" "encoded root"
	case $got in *"/x%20'%22%25%5C%C3%A9%E6%97%A5") ;; *) fail "fixed suffix missing: $got" ;; esac
}

t_newline() {
	py 'import os, sys; os.makedirs(os.path.join(sys.argv[1], "nl", "a\nb"))' "$C"
	for d in "$C"/nl/*; do :; done
	DEV=/x/dev
	eq "$(facts_of "$d")" "" "facts for a path with a line feed"
	eq "$(count)" 1 "events"
}

t_dev_root() {
	mkdir "$C/plain"
	DEV=/x/dev
	case $(facts_of "$C/plain") in *"ME-Project-Dev-Root: /x/dev"*) ;; *) fail "valid dev root not sent" ;; esac
	for bad in '' '${user_config.dev_root}' rel/x '/a b' '/a%b' '/a"b'; do
		DEV=$bad
		case $(facts_of "$C/plain") in *Dev-Root*) fail "dev root [$bad] sent" ;; esac
	done
}

t_no_git() {
	mkrepo "$C/r"
	rm "$RB/git"
	eq "$(facts_of "$C/r")" "ME-Project-Root: $(pct_oracle "$C/r")" "facts without git"
}

t_facts_unborn() {
	git init -q -b main "$C/r"
	eq "$(facts_of "$C/r")" "ME-Project-Root: $(pct_oracle "$C/r")
ME-Project-Branch: main" "unborn facts"
}

t_facts_layouts() {
	main=$C/$(printf 'mt x %s"\303\251' "'")
	git init -q -b main --separate-git-dir "$C/gd" "$main"
	git -C "$main" commit -q --allow-empty -m init
	git -C "$main" worktree add -q -b lw "$C/lw"
	eq "$(facts_of "$main" | sed -n 1p)" "ME-Project-Root: $(pct_oracle "$main")" "separate git dir, main tree"
	eq "$(facts_of "$C/lw" | sed -n 1p)" "ME-Project-Root: $(pct_oracle "$C/lw")" "separate git dir, linked worktree"
	mkrepo "$C/sub"
	mkrepo "$C/super"
	git -C "$C/super" -c protocol.file.allow=always submodule add -q "$C/sub" sm 2>/dev/null
	eq "$(facts_of "$C/super/sm" | sed -n 1p)" "ME-Project-Root: $(pct_oracle "$C/super/sm")" "submodule checkout"
	mkdir -p "$C/gds/p"
	git init -q -b main --separate-git-dir "$C/gds/p/.git" "$C/m2"
	git -C "$C/m2" commit -q --allow-empty -m init
	git -C "$C/m2" worktree add -q -b lw2 "$C/lw2"
	eq "$(facts_of "$C/lw2" | sed -n 1p)" "ME-Project-Root: $(pct_oracle "$C/gds/p")" "separate git dir named .git, linked worktree"
}

# ---------------------------------------------------------------- item 5: delivery

# stub_start NAME: start tests/stub_server.py in $C/NAME (its files steer it), with the token check on; prints the port.
stub_start() {
	mkdir -p "$C/$1"
	printf '%s' "$TOKV" >"$C/$1/token"
	python3 -I "$HERE/stub_server.py" "$C/$1" </dev/null >/dev/null 2>&1 &
	printf '%s\n' "$!" >"$C/$1.pid"
	_i=0
	while [ ! -s "$C/$1/port" ]; do
		_i=$((_i + 1))
		[ "$_i" -lt 50 ] || fail "stub $1 did not start"
		sleep 0.1
	done
	cat "$C/$1/port"
}

stub_lines() {
	if [ -f "$C/$1/log" ]; then wc -l <"$C/$1/log" | tr -d ' '; else echo 0; fi
}

calls() {
	_n=0
	for _c in "$FAKE_CURL_DIR"/calls/*; do
		[ -d "$_c" ] && _n=$((_n + 1))
	done
	printf '%s\n' "$_n"
}

reset_outbox() {
	settle
	rm -rf "$O" "$FAKE_CURL_DIR" "$TOKF"
	mkdir -p "$FAKE_CURL_DIR"
}

# queue BODY...: one capture per body, with no token so that the detached flushes hold, then settle.
queue() {
	for _b; do
		printf '%s' "$_b" >"$C/in"
		hook capture <"$C/in"
	done
	settle
}

t_status_classes() {
	for code in 204 400 409 413 422 408 429 500 503 000 401 403 200 201 202 302 307 404 405 410 418; do
		reset_outbox
		queue A B
		token
		printf '%s\n204\n' "$code" >"$FAKE_CURL_DIR/plan"
		flush_fg
		case $code in
		204) want="0 0 2" ;;
		400 | 409 | 413 | 422) want="0 1 2" ;;
		*) want="2 0 1" ;;
		esac
		eq "$(count) $(count "$O/rejected") $(calls)" "$want" "queued, parked and calls after $code"
		[ "$code" = 204 ] || [ "$(calls)" -eq 1 ] || [ "$(cat "$O/rejected"/*/body)" = A ] || fail "$code parked the wrong event"
		rm -rf "$FAKE_CURL_DIR"/calls "$FAKE_CURL_DIR/plan"
		flush_fg
		eq "$(count)" 0 "queue after a 204 flush following $code"
		for k in 1 2; do
			[ -f "$FAKE_CURL_DIR/calls/$k/body" ] || break
			case $code in 400 | 409 | 413 | 422) ;; *) eq "$(cat "$FAKE_CURL_DIR/calls/$k/body")" "$([ $k = 1 ] && echo A || echo B)" "delivery order after $code" ;; esac
		done
	done
}

t_curl_exit() {
	queue A
	token
	printf '204/18\n' >"$FAKE_CURL_DIR/plan"
	flush_fg
	eq "$(count) $(calls)" "1 1" "a 204 with a curl error"
}

t_queue_while_failing() {
	queue A B C
	token
	printf '000\n' >"$FAKE_CURL_DIR/plan"
	flush_fg
	eq "$(count) $(calls)" "3 1" "queued while curl fails"
	printf '204\n' >"$FAKE_CURL_DIR/plan"
	flush_fg
	eq "$(count) $(calls)" "0 4" "after success"
	eq "$(cat "$FAKE_CURL_DIR/calls/2/body")$(cat "$FAKE_CURL_DIR/calls/3/body")$(cat "$FAKE_CURL_DIR/calls/4/body")" ABC "order"
}

t_no_token() {
	queue A
	flush_fg
	eq "$(count) $(calls)" "1 0" "without a token file"
	token
	flush_fg
	eq "$(count) $(calls)" "0 1" "once the token exists"
}

t_token_channel() {
	queue A
	id=$(events)
	cp -R "$O/$id" "$C/queued"
	token
	flush_fg
	c=$FAKE_CURL_DIR/calls/1
	printf 'Authorization: Bearer %s\n' "$TOKV" >"$C/want-auth"
	cmp "$C/want-auth" "$c/stdin" || fail "curl stdin is not the Authorization line"
	cmp "$C/queued/headers" "$c/headers" && cmp "$C/queued/body" "$c/body" || fail "curl did not get the queued files"
	! grep -rq "$TOKV" "$FAKE_CURL_DIR"/calls/*/argv "$S" || fail "the token reached an argv or the state root"
	eq "$(sed -n 1p "$c/argv")" -q "curl's first argument"
	for want in -g --noproxy -H @- --connect-timeout 5 --speed-limit 8192 --speed-time 60; do
		grep -qx -e "$want" "$c/argv" || fail "argv lacks $want"
	done
	grep -qx '\*' "$c/argv" || fail "argv lacks the --noproxy value"
	! grep -qx -e -L -e --location -e --max-time -e -m "$c/argv" || fail "argv follows redirects or bounds total time"
	eq "$(tail -n 1 "$c/argv")" "$URL/lifecycle/events" "url"
}

t_token_forms() {
	queue A
	mkdir -p "${TOKF%/*}"
	printf '%s' "$TOKV" >"$TOKF"
	flush_fg
	printf 'Authorization: Bearer %s\n' "$TOKV" >"$C/want-auth"
	cmp "$C/want-auth" "$FAKE_CURL_DIR/calls/1/stdin" || fail "a token file without its line feed"
	rm -f "$TOKF"
	queue B
	: >"$TOKF"
	flush_fg
	eq "$(count) $(calls)" "1 1" "an empty token file holds"
}

t_endpoint_update_fail() {
	token
	printf '000\n' >"$FAKE_CURL_DIR/plan"
	hook capture <"$C/in"
	settle
	wrap mv 'case $1 in *.endpoint) exit 1 ;; esac'
	rm -rf "$FAKE_CURL_DIR/calls"
	hook capture https://example.com/new <"$C/in"
	settle
	[ ! -e "$O/endpoint" ] || fail "the old endpoint survived a failed update"
	flush_fg
	eq "$(count) $(calls)" "2 0" "no request without an endpoint"
}

t_endpoint_changed_mid_flush() {
	queue A B C
	token
	printf 'rm -f "%s/endpoint"\n' "$O" >"$FAKE_CURL_DIR/oncall.1"
	flush_fg
	eq "$(count) $(calls)" "2 1" "after the endpoint vanished mid-flush"
	printf 'http://127.0.0.1:9\n' >"$O/endpoint"
	rm -rf "$FAKE_CURL_DIR"/calls "$FAKE_CURL_DIR"/oncall.1
	printf 'printf "https://example.com/other\\n" >"%s/endpoint"\n' "$O" >"$FAKE_CURL_DIR/oncall.1"
	flush_fg
	eq "$(count) $(calls)" "1 1" "after the endpoint changed mid-flush"
}

t_parked_kept() {
	queue A
	token
	printf '422\n' >"$FAKE_CURL_DIR/plan"
	flush_fg
	eq "$(count "$O/rejected")" 1 "parked"
	out=$(find "$O/rejected" -type f ! -perm 600)
	[ -z "$out" ] || fail "parked files not 0600: $out"
	printf '204\n' >"$FAKE_CURL_DIR/plan"
	flush_fg
	eq "$(count "$O/rejected") $(calls)" "1 1" "a parked event is not sent again"
}

t_park_heals() {
	queue A
	token
	rm -rf "$O/rejected"
	printf '400\n' >"$FAKE_CURL_DIR/plan"
	flush_fg
	id=$(events "$O/rejected")
	[ -f "$O/rejected/$id/body" ] && [ -f "$O/rejected/$id/headers" ] || fail "the park did not land as rejected/<id>"
}

t_detached() {
	token
	printf 'block\n' >"$FAKE_CURL_DIR/plan"
	t0=$(date +%s)
	out=$(PATH=$RB /bin/sh "$HB" capture "$URL" "" <"$C/in")
	t1=$(date +%s)
	: >"$FAKE_CURL_DIR/release"
	eq "$out" "" "capture output"
	[ $((t1 - t0)) -le 2 ] || fail "capture took $((t1 - t0)) s while its flush blocked"
	settle
}

t_real_curl() {
	rm "$RB/curl"
	ln -s "$(command -v curl)" "$RB/curl"
	port=$(stub_start stub)
	URL=http://127.0.0.1:$port
	printf 503 >"$C/stub/status"
	token
	hook capture <"$C/in"
	settle
	id=$(events)
	cp -R "$O/$id" "$C/queued"
	printf 204 >"$C/stub/status"
	flush_fg
	eq "$(count)" 0 "queue"
	n=$(stub_lines stub)
	cmp "$C/queued/body" "$C/stub/req/$n.body" || fail "body differs"
	while IFS= read -r l; do
		grep -qxF "$l" "$C/stub/req/$n.headers" || grep -qxF "$l$(printf '\r')" "$C/stub/req/$n.headers" || fail "header [$l] not sent"
	done <"$C/queued/headers"
	grep -q "^Authorization: Bearer $TOKV" "$C/stub/req/$n.headers" || fail "no Authorization"
}

t_redirect() {
	rm "$RB/curl"
	ln -s "$(command -v curl)" "$RB/curl"
	port=$(stub_start stub)
	URL=http://127.0.0.1:$port
	printf 307 >"$C/stub/status"
	printf 'location\n' >"$HOME/.curlrc"
	queue A
	token
	flush_fg
	eq "$(stub_lines stub) $(count)" "1 1" "requests and queue after a 307"
}

t_proxy_bypassed() {
	rm "$RB/curl"
	ln -s "$(command -v curl)" "$RB/curl"
	port=$(stub_start stub)
	pport=$(stub_start proxy)
	URL=http://127.0.0.1:$port
	queue A
	token
	http_proxy=http://127.0.0.1:$pport HTTP_PROXY=http://127.0.0.1:$pport ALL_PROXY=http://127.0.0.1:$pport
	export http_proxy HTTP_PROXY ALL_PROXY
	flush_fg
	eq "$(stub_lines stub) $(stub_lines proxy) $(count)" "1 0 0" "server, proxy, queue"
}

t_no_curl() {
	rm "$RB/curl"
	token
	hook capture <"$C/in"
	settle
	eq "$(count)" 1 "events without curl"
}

# ---------------------------------------------------------------- runner

CASES=${CASES:-"
registration version
body_identical envelope order seq_taken seq_bound pid_reuse id_collision rejected_collision tree_heals
body_write_fail endpoint side_duties_fail silent modes
facts_repo facts_worktree facts_subdir facts_detached facts_non_git facts_cwd facts_insteadof userinfo encoding
newline dev_root no_git facts_unborn facts_layouts
status_classes curl_exit queue_while_failing no_token token_channel token_forms endpoint_update_fail
endpoint_changed_mid_flush parked_kept park_heals detached real_curl redirect proxy_bypassed no_curl
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
	for f in "$C"/stub*.pid; do [ -f "$f" ] && kill "$(cat "$f")" 2>/dev/null; done
	pkill -f "$C/plugin/bin/me-hook" 2>/dev/null
done
printf '# %d ok, %d not ok\n' "$OK" "$BAD"
[ "$BAD" -eq 0 ]
