"""Measure a capture hook's latency against the 50 ms p95 budget (ADR-0020 §12, #1019).

python3 -I latency.py [--steps]

Builds a scratch HOME with a token file, a checkout with origin, upstream and a linked worktree (the worktree is the
CLAUDE_PROJECT_DIR, so the capture makes every git call), and tests/stub_server.py answering 204. It runs 10
unmeasured and 200 measured captures, 100 ms apart, each timed from spawn until exit and EOF on stdout and stderr:
that is what Claude Code waits for. Exits 1 when p95 exceeds 50 ms or a capture printed or failed. --steps also times
each external command one capture makes.
"""

import os
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
HELPER = os.path.join(os.path.dirname(HERE), "bin", "me-hook")
BUDGET_MS = 50.0


def git(*args, cwd):
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, env=ENV)


def percentile(values, p):
    ordered = sorted(values)
    return ordered[max(0, int(round(p / 100.0 * len(ordered) + 0.5)) - 1)]


root = os.path.realpath(tempfile.mkdtemp(prefix="me-latency."))
ENV = {
    "HOME": os.path.join(root, "home"),
    "PATH": "/usr/bin:/bin",
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_AUTHOR_NAME": "t",
    "GIT_AUTHOR_EMAIL": "t@example.com",
    "GIT_COMMITTER_NAME": "t",
    "GIT_COMMITTER_EMAIL": "t@example.com",
}
stub = None
try:
    os.makedirs(os.path.join(ENV["HOME"], ".config", "memory-engine", "secrets"))
    with open(
        os.path.join(ENV["HOME"], ".config", "memory-engine", "secrets", "token"), "w"
    ) as f:
        f.write("me_" + "0" * 32 + "\n")
    repo = os.path.join(root, "dev", "proj")
    os.makedirs(repo)
    git("init", "-q", "-b", "main", cwd=repo)
    git("commit", "-q", "--allow-empty", "-m", "init", cwd=repo)
    git("remote", "add", "origin", "https://example.com/example/proj.git", cwd=repo)
    git("remote", "add", "upstream", "git@example.com:example/upstream.git", cwd=repo)
    worktree = os.path.join(root, "dev", "proj-wt")
    git("worktree", "add", "-q", "-b", "wt", worktree, cwd=repo)

    stub_dir = os.path.join(root, "stub")
    os.makedirs(stub_dir)
    stub = subprocess.Popen(
        [sys.executable, "-I", os.path.join(HERE, "stub_server.py"), stub_dir],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    for _ in range(50):
        if os.path.exists(os.path.join(stub_dir, "port")):
            break
        time.sleep(0.1)
    with open(os.path.join(stub_dir, "port")) as f:
        url = "http://127.0.0.1:%s" % f.read().strip()

    body = (
        b'{"hook_event_name":"PostToolUse","tool_name":"Read","tool_input":{"file_path":"/home/example/dev/'
        + b"x" * 4000
        + b'"}}'
    )
    # The plugin's options reach the helper as Claude Code exports them, in the environment.
    env = dict(ENV, CLAUDE_PROJECT_DIR=worktree, CLAUDE_PLUGIN_OPTION_ME_URL=url,
               CLAUDE_PLUGIN_OPTION_DEV_ROOT=os.path.join(root, "dev"))
    argv = ["/bin/sh", HELPER, "capture"]
    times, bad = [], 0
    for i in range(210):
        t0 = time.perf_counter_ns()
        r = subprocess.run(argv, input=body, capture_output=True, env=env)
        ms = (time.perf_counter_ns() - t0) / 1e6
        if r.returncode != 0 or r.stdout or r.stderr:
            bad += 1
        if i >= 10:
            times.append(ms)
        time.sleep(0.1)

    real_sh = os.path.realpath("/bin/sh")
    print(
        "capture: n=%d p50=%.1f p95=%.1f max=%.1f ms (budget p95 <= %.0f), /bin/sh -> %s"
        % (
            len(times),
            percentile(times, 50),
            percentile(times, 95),
            max(times),
            BUDGET_MS,
            real_sh,
        )
    )

    if "--steps" in sys.argv[1:]:
        steps = [
            (
                "git rev-parse",
                [
                    "git",
                    "-C",
                    worktree,
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-dir",
                    "--git-common-dir",
                    "--show-toplevel",
                ],
            ),
            ("git symbolic-ref", ["git", "-C", worktree, "symbolic-ref", "-q", "HEAD"]),
            (
                "git remote get-url",
                ["git", "-C", worktree, "remote", "get-url", "origin"],
            ),
            ("date", ["date", "-u", "+%s %Y-%m-%dT%H:%M:%SZ"]),
            ("cat", ["cat", HELPER]),
            ("sh startup", ["/bin/sh", "-c", ":"]),
        ]
        for name, cmd in steps:
            samples = []
            for _ in range(50):
                t0 = time.perf_counter_ns()
                subprocess.run(cmd, capture_output=True, env=ENV)
                samples.append((time.perf_counter_ns() - t0) / 1e6)
            print("  %-20s p50=%.1f ms" % (name, percentile(samples, 50)))

    # The measured captures must have been configured: their detached flushes reached the stub.
    time.sleep(2)
    log = os.path.join(stub_dir, "log")
    delivered = sum(1 for _ in open(log)) if os.path.exists(log) else 0
    print("stub received %d requests" % delivered)
    if delivered == 0:
        bad += 1
    if bad:
        print("%d captures printed or failed" % bad)
    sys.exit(1 if bad or percentile(times, 95) > BUDGET_MS else 0)
finally:
    if stub is not None:
        stub.terminate()
    time.sleep(1)
    shutil.rmtree(root, ignore_errors=True)
