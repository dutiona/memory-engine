"""The MCP phases of real-claude.sh: a local memory-engine and knowledge-base, driven by a real `claude -p`.

python3 -I real_mcp.py CONFIG. CONFIG is a JSON file real-claude.sh writes: the phase directory, the scratch HOME,
the binaries, the project directories, the two loopback ports and the turns. This script starts the stub embedder
and the servers, draws the machine tokens, drives the session turn by turn through claude's stream-json input,
restarts and rotates between turns, and leaves its evidence in <phase dir>/out/ for the checks:

- streams/<n>.jsonl, one claude process each; turns.json, each turn's tool results;
- export.json, `memory-engine-cli export` of the stopped ME store; kb-search.json, KB answers;
- precondition, the first turn whose expected tool call never came (`model did not call <tool>`), if any.

The Claude token reaches claude through its environment only, as in real-claude.sh's launcher. Machine tokens are
drawn here, written to their files and never printed.
"""

import hashlib
import json
import os
import queue
import secrets
import signal
import socket
import string
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

CFG = json.load(open(sys.argv[1]))
DRAWN = []
P = CFG["dir"]
OUT = os.path.join(P, "out")
os.makedirs(OUT, exist_ok=True)
H = CFG["home"]
TOKEN_FILE = os.path.join(H, ".config", "memory-engine", "secrets", "token")
HASHES = os.path.join(P, "tokens")
MACHINE = CFG["machine"]
PROCS = {}
TURN_SECONDS = 300


def draw_token():
    """A fresh ADR-0020 §4 token: the client file and the one hash line both servers read."""
    token = "me_" + "".join(
        secrets.choice(string.digits + string.ascii_letters) for _ in range(32)
    )
    DRAWN.append(token)
    os.makedirs(os.path.dirname(TOKEN_FILE), exist_ok=True)
    for path, data in (
        (TOKEN_FILE, token + "\n"),
        (HASHES, "%s %s\n" % (MACHINE, hashlib.sha256(token.encode()).hexdigest())),
    ):
        tmp = path + ".tmp"
        with open(os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600), "w") as f:
            f.write(data)
        os.replace(tmp, path)
    return token


def wait_port(path):
    for _ in range(100):
        if os.path.exists(path):
            return int(open(path).read())
        time.sleep(0.1)
    raise SystemExit("no port in " + path)


def status(port, token=None):
    """The status of a bare POST /mcp, with the token when given; 0 when nothing answers."""
    req = urllib.request.Request(
        "http://127.0.0.1:%d/mcp" % port,
        data=b"{}",
        method="POST",
        headers={
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
        },
    )
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        return urllib.request.urlopen(req, timeout=5).status
    except urllib.error.HTTPError as e:
        return e.code
    except OSError:
        return 0


def start(name):
    log = open(os.path.join(P, name + ".log"), "a")
    env = {"HOME": H, "PATH": "/usr/bin:/bin"}
    if name == "me":
        argv = [
            CFG["me_bin"],
            "--listen",
            "127.0.0.1:%d" % CFG["me_port"],
            "--token-file",
            HASHES,
            "--user",
            "tester",
            "--db-path",
            os.path.join(P, "me", "memory.db"),
            "--embed-dim",
            "16",
            "--embed-url",
            "http://127.0.0.1:%d/v1/embeddings" % EMBED_PORT,
            "--embed-model",
            "stub",
            "--embed-provider",
            "openai",
        ]
    else:
        argv = [
            CFG["kb_bin"],
            "--http",
            "127.0.0.1:%d" % CFG["kb_port"],
            "--token-file",
            HASHES,
        ]
        env.update(
            {
                "KNOWLEDGE_BASE_DB": os.path.join(P, "kb", "kb.db"),
                "KNOWLEDGE_BASE_DATA_DIR": os.path.join(P, "kb", "data"),
                "KNOWLEDGE_BASE_EMBED_PROVIDER": "openai",
                "KNOWLEDGE_BASE_EMBED_MODEL": "stub",
                "KNOWLEDGE_BASE_EMBED_DIM": "16",
                "KNOWLEDGE_BASE_EMBED_URL": "http://127.0.0.1:%d" % EMBED_PORT,
            }
        )
    PROCS[name] = subprocess.Popen(
        argv,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=log,
        start_new_session=True,
    )
    port = CFG[name + "_port"]
    for _ in range(300):
        if status(port) == 401:
            return
        if PROCS[name].poll() is not None:
            raise SystemExit(
                "%s exited %s; see %s.log" % (name, PROCS[name].returncode, name)
            )
        time.sleep(0.1)
    raise SystemExit(name + " not ready in 30 s")


def stop(name):
    """SIGTERM the child's process group, then SIGKILL it after 60 s. Never raises, so a final cleanup reaches
    every child."""
    proc = PROCS.get(name)
    if proc is None:
        return
    for sig, grace in ((signal.SIGTERM, 60), (signal.SIGKILL, None)):
        try:
            os.killpg(proc.pid, sig)
        except ProcessLookupError:
            pass
        try:
            proc.wait(timeout=grace)
            break
        except subprocess.TimeoutExpired:
            pass
    del PROCS[name]


def servers():
    return [s for s in ("me", "kb") if CFG.get(s + "_port")]


def restart():
    for s in servers():
        stop(s)
    for s in servers():
        start(s)


def rotate():
    for s in servers():
        stop(s)
    old, new = DRAWN[-1], draw_token()
    for s in servers():
        start(s)
    # What each server answers a bare POST /mcp with no token, the old one and the new one, after the rotation.
    json.dump(
        {
            s: [
                status(CFG[s + "_port"]),
                status(CFG[s + "_port"], old),
                status(CFG[s + "_port"], new),
            ]
            for s in servers()
        },
        open(os.path.join(OUT, "rotation.json"), "w"),
    )


def claude_env():
    with open(CFG["claude_token_file"]) as f:
        token = f.read().strip().rpartition("=")[2]
    return {
        "HOME": H,
        "PATH": CFG["path"],
        "LANG": "C.UTF-8",
        "DISABLE_AUTOUPDATER": "1",
        "CLAUDE_CODE_OAUTH_TOKEN": token,
        "PWD": CFG["pwd"],
    }


def tool_calls(lines):
    """(name, input) of every tool_use, and {tool_use_id: (is_error, text)} of every tool_result, in a stream."""
    uses, results = [], {}
    for msg in lines:
        content = (msg.get("message") or {}).get("content")
        if not isinstance(content, list):
            continue
        for item in content:
            if item.get("type") == "tool_use":
                uses.append((item["id"], item["name"], item.get("input", {})))
            elif item.get("type") == "tool_result":
                body = item.get("content")
                if isinstance(body, list):
                    body = "".join(
                        c.get("text", "") for c in body if isinstance(c, dict)
                    )
                results[item["tool_use_id"]] = (bool(item.get("is_error")), body or "")
    return uses, results


TURNS = []


def record_turn(n, turn, lines):
    uses, results = tool_calls(lines)
    calls = []
    for want_name, nonce in turn["expect"]:
        found = [
            u
            for u in uses
            if u[1] == want_name and nonce in json.dumps(u[2], ensure_ascii=False)
        ]
        if not found:
            open(os.path.join(OUT, "precondition"), "w").write(
                "turn %d: model did not call %s with %s\n" % (n, want_name, nonce)
            )
            return False
        err, text = results.get(found[0][0], (True, "<no tool result>"))
        calls.append({"tool": want_name, "nonce": nonce, "is_error": err, "text": text})
    TURNS.append({"turn": n, "label": turn.get("label", ""), "calls": calls})
    json.dump(
        TURNS, open(os.path.join(OUT, "turns.json"), "w"), indent=1, ensure_ascii=False
    )
    return True


def session(argv, turns, stream_n):
    """One claude process; with turns, a stream-json session fed one turn at a time."""
    out = open(os.path.join(OUT, "streams", "%d.jsonl" % stream_n), "w")
    proc = subprocess.Popen(
        argv,
        env=claude_env(),
        cwd=CFG["pwd"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=open(os.path.join(OUT, "streams", "%d.err" % stream_n), "w"),
        text=True,
        start_new_session=True,
    )
    PROCS["claude"] = proc
    q = queue.Queue()

    def read():
        for raw in proc.stdout:
            q.put(raw)
        q.put(None)

    threading.Thread(target=read, daemon=True).start()
    ok = True
    timed_out = False
    for n, turn in turns:
        msg = {"type": "user", "message": {"role": "user", "content": turn["prompt"]}}
        proc.stdin.write(json.dumps(msg) + "\n")
        proc.stdin.flush()
        lines = []
        deadline = time.time() + TURN_SECONDS
        timed_out = False
        while True:
            try:
                raw = q.get(timeout=max(0.0, deadline - time.time()))
            except queue.Empty:
                open(os.path.join(OUT, "precondition"), "w").write(
                    "turn %d: no result within %d s\n" % (n, TURN_SECONDS)
                )
                timed_out = True
                break
            if raw is None:
                break
            out.write(raw)
            out.flush()
            try:
                lines.append(json.loads(raw))
            except ValueError:
                continue
            if lines[-1].get("type") == "result":
                break
        if timed_out:
            ok = False
            break
        if not record_turn(n, turn, lines):
            ok = False
            break
        if turn.get("after") == "restart":
            restart()
        elif turn.get("after") == "rotate":
            rotate()
    proc.stdin.close()
    if timed_out:
        stop("claude")
    drain_deadline = time.time() + 60
    while time.time() < drain_deadline:
        try:
            raw = q.get(timeout=max(0.0, drain_deadline - time.time()))
        except queue.Empty:
            break
        if raw is None:
            break
        out.write(raw)
    out.flush()
    try:
        proc.wait(timeout=60)
    except subprocess.TimeoutExpired:
        pass
    stop("claude")
    return ok


def kb_search(query):
    token = open(TOKEN_FILE).read().strip()
    body = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "search_index", "arguments": {"query": query}},
    }
    req = urllib.request.Request(
        "http://127.0.0.1:%d/mcp" % CFG["kb_port"],
        data=json.dumps(body).encode(),
        method="POST",
        headers={
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
            "Authorization": "Bearer " + token,
        },
    )
    return json.load(urllib.request.urlopen(req, timeout=60))


def flush():
    subprocess.run(
        [
            "/bin/sh",
            os.path.join(H, ".local", "state", "memory-engine", "bin", "me-hook"),
            "flush",
        ],
        env={},
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=300,
    )


def main():
    global EMBED_PORT
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    for d in ("me", "kb", "emb", os.path.join("out", "streams")):
        os.makedirs(os.path.join(P, d), exist_ok=True)
    emb = subprocess.Popen(
        [sys.executable, "-I", CFG["stub_embedder"], os.path.join(P, "emb"), "16"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        EMBED_PORT = wait_port(os.path.join(P, "emb", "port"))
        first = draw_token()
        for s in servers():
            start(s)
        json.dump(
            {
                s: [status(CFG[s + "_port"]), status(CFG[s + "_port"], first)]
                for s in servers()
            },
            open(os.path.join(OUT, "auth.json"), "w"),
        )
        base = [
            CFG["claude"],
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--max-turns",
            "40",
            "--allowedTools",
            ",".join(CFG["allowed_tools"]),
        ] + sum((["--add-dir", d] for d in CFG.get("add_dirs", [])), [])
        turns = list(enumerate(CFG["turns"], 1))
        ok = session(base + ["--input-format", "stream-json"], turns, 1)
        if ok and CFG.get("resume"):
            init = next(
                json.loads(l)
                for l in open(os.path.join(OUT, "streams", "1.jsonl"))
                if '"init"' in l
            )
            ok = session(
                base
                + ["--input-format", "stream-json", "--resume", init["session_id"]],
                [(len(turns) + 1, CFG["resume"])],
                2,
            )
        if "kb" in servers():
            json.dump(
                {q: kb_search(q) for q in CFG.get("kb_queries", [])},
                open(os.path.join(OUT, "kb-search.json"), "w"),
            )
        flush()
    finally:
        for s in list(PROCS):
            stop(s)
        emb.terminate()
    subprocess.run(
        [
            CFG["cli_bin"],
            "--db",
            os.path.join(P, "me", "memory.db"),
            "export",
            "-o",
            os.path.join(OUT, "export.json"),
        ],
        env={"PATH": "/usr/bin:/bin"},
        stdout=subprocess.DEVNULL,
        stderr=open(os.path.join(OUT, "export.err"), "w"),
        check=False,
    )
    leaks(claude_env()["CLAUDE_CODE_OAUTH_TOKEN"])
    return 0 if ok else 1


def leaks(claude_token):
    """out/leaks.json: every file under the phase directory that holds a machine token, outside the token file,
    or the Claude token. Paths only, never the values."""
    found = []
    needles = [t.encode() for t in DRAWN] + [claude_token.encode()]
    for root, _, files in os.walk(P):
        for name in files:
            path = os.path.join(root, name)
            if path in (TOKEN_FILE, os.path.join(OUT, "leaks.json")) or os.path.islink(
                path
            ):
                continue
            try:
                data = open(path, "rb").read()
            except OSError:
                continue
            if any(n in data for n in needles):
                found.append(os.path.relpath(path, P))
    json.dump(
        {"drawn": len(DRAWN), "files": found},
        open(os.path.join(OUT, "leaks.json"), "w"),
    )


# ---------------------------------------------------------------- checks, one per real-claude.sh line

ME = "mcp__plugin_memory-engine_me__"
KB = "mcp__plugin_memory-engine_kb__"
SEVEN = {ME + t for t in ("recall", "remember", "forget", "set_level")} | {
    KB + t for t in ("search_index", "ingest_text", "ingest_url")
}


def load(name):
    return json.load(open(os.path.join(OUT, name)))


def inits():
    out = []
    for n in sorted(os.listdir(os.path.join(OUT, "streams"))):
        if n.endswith(".jsonl"):
            out += [
                json.loads(l)
                for l in open(os.path.join(OUT, "streams", n))
                if '"subtype":"init"' in l.replace(" ", "")
            ]
    return out


def calls():
    return {(t["label"], c["tool"]): c for t in load("turns.json") for c in t["calls"]}


def expected_key():
    dev = os.path.realpath(CFG["dev_root"])
    return CFG["machine"] + ":" + os.path.relpath(os.path.realpath(CFG["project"]), dev)


def events():
    return load("export.json")["events"]


def check(name):
    n = CFG["nonce"]
    if name == "tools":
        for init in inits():
            servers = {s["name"]: s["status"] for s in init["mcp_servers"]}
            assert servers == CFG["want_servers"], servers
            tools = {t for t in init["tools"] if t.startswith("mcp__")}
            assert tools == {
                t
                for t in SEVEN
                if CFG["want_servers"].get("plugin:memory-engine:" + t.split("_")[4])
                == "connected"
            }, sorted(tools)
        print("init events:", len(inits()))
    elif name == "calls":
        for (label, tool), c in calls().items():
            assert not c["is_error"], (label, tool, c["text"][:200])
            if tool.startswith(ME):
                assert (
                    len(c["text"]) > 21
                    and c["text"][:5] == "call "
                    and all(x in "0123456789abcdef" for x in c["text"][5:21])
                    and c["text"][21] == "\n"
                ), c["text"][:40]
    elif name == "key":
        key = expected_key()
        for label, tool in [k for k in calls() if k[1] == ME + "remember"]:
            assert "project " + key + "," in calls()[(label, tool)]["text"], calls()[
                (label, tool)
            ]["text"][:120]
        root, dev = os.path.realpath(CFG["project"]), os.path.realpath(CFG["dev_root"])
        for e in events():
            pr = e["payload"].get("project") or {}
            assert pr.get("key") == key, (e["id"], e["source"], pr)
            assert (
                pr["facts"].get("root") == root and pr["facts"].get("dev_root") == dev
            ), (e["id"], pr["facts"])
        print("key:", key, "on", len(events()), "events and call records")
    elif name == "restart":
        fact = (
            calls()[("T1", ME + "remember")]["text"]
            .split("stored ", 1)[1]
            .split(" ", 1)[0]
        )
        for label in ("T2", "T3"):
            got = json.loads(calls()[(label, ME + "recall")]["text"].split("\n", 1)[1])[
                "results"
            ]
            assert any(
                str(r["id"]) == fact
                and ("alpha-" + n) in r["content"]
                and r["scope"] == expected_key()
                for r in got
            ), got
            assert (
                "content://%s/note-%s" % (CFG["machine"], n)
                in calls()[(label, KB + "search_index")]["text"]
            )
    elif name == "session":
        ev = {e["id"]: e for e in events()}
        sessions = {i["session_id"] for i in inits()}
        records = [e for e in events() if e["source"] == "mcp"]
        linked = {
            r["call_event"]: ev[r["event_row"]]
            for r in load("export.json")["lifecycle_receipts"]
            if r["call_event"]
        }
        assert records and all(
            r["id"] in linked and linked[r["id"]]["session_id"] in sessions
            for r in records
        ), (records, linked)
        kb = [
            e
            for e in events()
            if e["source"] == "lifecycle"
            and str(e["payload"].get("tool", "")).startswith(KB)
        ]
        assert kb and all(e["session_id"] in sessions for e in kb), kb
        print("call records linked:", len(records), "; KB calls captured:", len(kb))
    elif name == "auth":
        first = load("auth.json")
        assert all(first[s][0] == 401 and first[s][1] != 401 for s in first), first
        after = load("rotation.json")
        assert all(
            after[s][0] == 401 and after[s][1] == 401 and after[s][2] != 401
            for s in after
        ), after
        print("before:", first, "after the rotation:", after)
    elif name == "rotation":
        assert load("leaks.json")["drawn"] == 2, load("leaks.json")
        assert (
            not calls()[("T3", ME + "recall")]["is_error"]
            and not calls()[("T3", KB + "search_index")]["is_error"]
        )
    elif name == "cd":
        assert calls()[("T4", "Bash")]["text"].strip() == os.path.realpath(
            CFG["add_dirs"][0]
        ), calls()[("T4", "Bash")]["text"]
        after = [
            e
            for e in events()
            if e["source"] == "lifecycle"
            and e["payload"].get("cwd") == os.path.realpath(CFG["add_dirs"][0])
        ]
        assert after, "no event after the cd carries the other repository as cwd"
        assert (
            "project " + expected_key() + ","
            in calls()[("T4", ME + "remember")]["text"]
        )
        assert (
            "example.com/example/other"
            not in open(os.path.join(OUT, "export.json")).read()
        )
    elif name == "resume":
        streams = sorted(
            n for n in os.listdir(os.path.join(OUT, "streams")) if n.endswith(".jsonl")
        )
        assert len(streams) == 2, streams
        assert len({i["session_id"] for i in inits()}) == 1, [
            i["session_id"] for i in inits()
        ]
        got = calls()[("R1", ME + "recall")]["text"]
        assert ("charlie-" + n) in got and expected_key() in got, got[:300]
    elif name == "leaks":
        assert not load("leaks.json")["files"], load("leaks.json")
    elif name == "unscoped":
        text = calls()[("L1", ME + "remember")]["text"]
        assert "unscoped" in text, text[:200]
        for e in events():
            assert (e["payload"].get("project") or {}).get("key") is None, (
                e["id"],
                e["payload"].get("project"),
            )
        print("L:", text.split("\n", 1)[1][:100])
    elif name == "cwd-info":
        start = [e for e in events() if e["payload"].get("kind") == "session_start"][0][
            "payload"
        ]["cwd"]
        print(
            "# info: launched from %s; the hooks saw cwd %s (%s)"
            % (
                CFG["pwd"],
                start,
                "physical" if start == os.path.realpath(start) else "logical",
            )
        )
    else:
        raise SystemExit("unknown check " + name)
    return 0


EMBED_PORT = 0
sys.exit(main() if len(sys.argv) == 2 else check(sys.argv[2]))
