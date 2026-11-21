"""Turn a capture.sh outbox into checked-in fixtures, or check fixtures for local data.

    python3 -I normalize.py CAPTURE FIXTURES     # CAPTURE is capture.sh's OUT directory
    python3 -I normalize.py --check FIXTURES     # exits 1 when a fixture holds local data

Bodies are rewritten as bytes, never re-serialized, so each fixture stays the hook input Claude Code produced, with
only these substitutions: the scratch HOME becomes /work/fixture-home, and the rest of the scratch root, literal and
in Claude Code's dash-encoded project-directory form, /work/fixture; each value in REPLACED_KEYS gets a fixed placeholder of its shape, so no redaction rule or scanner
mistakes an identifier for a secret. The first event of each fixture name is kept. Local data is the scratch root,
$HOME, the user name, the host name and the Claude OAuth token; a finding names the file and never the value.
"""

import glob
import json
import os
import pwd
import re
import socket
import sys

ROOT = "/work/fixture"
# Opaque high-entropy identifiers Claude Code mints; their values carry no meaning for the tests.
REPLACED_KEYS = {"tool_use_id": "toolu_fixture{:04d}"}


def kebab(name):
    return re.sub(r"(?<!^)(?=[A-Z])", "-", name).lower()


def fixture_name(body):
    hook = body["hook_event_name"]
    name = kebab(hook)
    if hook == "SessionStart":
        name += "-" + body.get("source", "none")
    if "tool_name" in body:
        name += "-" + re.sub(r"[^a-z0-9]+", "-", body["tool_name"].lower()).strip("-")
    if "agent_id" in body and not hook.startswith("Subagent"):
        name += "-subagent"
    if hook == "SubagentStop" and not body.get("agent_type"):
        name += "-internal"
    return name


def local_values(scratch):
    values = {
        scratch,
        os.path.expanduser("~"),
        pwd.getpwuid(os.getuid()).pw_name,
        socket.gethostname(),
    }
    token_file = os.environ.get(
        "ME_TEST_CLAUDE_TOKEN_FILE",
        os.path.expanduser("~/.config/memory-engine/test-secrets/claude-oauth-token"),
    )
    try:
        with open(token_file) as f:
            values.add(f.read().strip().rpartition("=")[2])
    except OSError:
        pass
    return {v for v in values if v}


def leaks(path, values):
    data = open(path, "rb").read()
    return [path for v in values if v.encode() in data]


def check(fixtures, values):
    bad = []
    for path in sorted(glob.glob(os.path.join(fixtures, "*", "*"))):
        bad += leaks(path, values)
        if b"/tmp/" in open(path, "rb").read():
            bad.append(path)
    for path in sorted(set(bad)):
        print("local data in", path, file=sys.stderr)
    return 1 if bad else 0


def main():
    if sys.argv[1] == "--check":
        return check(sys.argv[2], local_values("/tmp/me-capture."))
    capture, fixtures = sys.argv[1], sys.argv[2]
    scratch = open(os.path.join(capture, "scratch-root")).read().strip()
    dashed = re.sub(r"[/.]", "-", scratch)
    counters = {}
    seen = set()
    for event in sorted(glob.glob(os.path.join(capture, "outbox", "[0-9]*"))):
        raw = open(os.path.join(event, "body"), "rb").read()
        name = fixture_name(json.loads(raw))
        if name in seen:
            continue
        seen.add(name)
        out = []
        for part in ("body", "headers"):
            data = open(os.path.join(event, part), "rb").read()
            data = data.replace((scratch + "/home").encode(), (ROOT + "-home").encode())
            data = data.replace(scratch.encode(), ROOT.encode()).replace(
                dashed.encode(), b"-work-fixture"
            )
            for key, shape in REPLACED_KEYS.items():

                def repl(m, key=key, shape=shape):
                    counters[key] = counters.get(key, 0) + 1
                    return m.group(1) + shape.format(counters[key]).encode() + b'"'

                data = re.sub(rb'("' + key.encode() + rb'":")[^"]*"', repl, data)
            out.append((part, data))
        os.makedirs(os.path.join(fixtures, name), exist_ok=True)
        for part, data in out:
            with open(os.path.join(fixtures, name, part), "wb") as f:
                f.write(data)
    return check(fixtures, local_values(scratch))


if __name__ == "__main__":
    sys.exit(main())
