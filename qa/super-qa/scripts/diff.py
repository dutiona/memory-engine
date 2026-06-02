#!/usr/bin/env python3
"""Heuristic diff between two super-qa consolidated runs (longitudinal QA).

Usage:
    python3 diff.py <old/consolidated.json> <new/consolidated.json> [--out diff.md]

Matching is FUZZY by necessity: finding IDs and line numbers are not stable
across runs (agent-generated slugs; code shifts over months). A finding is
matched if it shares the same source FILE (path without :line) AND its title
tokens overlap >= JACCARD with a candidate in the other run. Reports:
  - added     : in NEW, no match in OLD  (newly surfaced)
  - removed   : in OLD, no match in NEW  (fixed, or no longer detected)
  - persisted : matched in both          (still open; flags severity changes)
Treat counts as directional, not exact. Tighten/loosen with --jaccard.
"""

import json, re, sys, argparse, collections

SEV = ["blocker", "critical", "high", "medium", "low", "info"]
STOP = set(
    "the a an of to in for and or is are be with no not its on at by via "
    "that this can use using value via from into has have".split()
)


def load(p):
    d = json.load(open(p))
    return d["findings"] if isinstance(d, dict) and "findings" in d else d


def sev(f):
    s = str(f.get("severity", "info")).lower()
    return s if s in SEV else "info"


def file_of(f):
    loc = str(f.get("location", ""))
    m = re.match(r"([^\s:]+\.rs)", loc)
    return m.group(1) if m else (loc.split(":")[0] if loc else "?")


def toks(f):
    return {
        w
        for w in re.findall(r"[a-z_]{3,}", str(f.get("title", "")).lower())
        if w not in STOP
    }


def match(new, old_by_file, jac):
    cands = old_by_file.get(file_of(new), [])
    nt = toks(new)
    best, bestscore = None, 0.0
    for o in cands:
        ot = toks(o)
        if not nt or not ot:
            continue
        j = len(nt & ot) / len(nt | ot)
        if j > bestscore:
            bestscore, best = j, o
    return best if bestscore >= jac else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("old")
    ap.add_argument("new")
    ap.add_argument("--jaccard", type=float, default=0.5)
    ap.add_argument("--out")
    a = ap.parse_args()
    old, new = load(a.old), load(a.new)

    old_by_file = collections.defaultdict(list)
    for f in old:
        old_by_file[file_of(f)].append(f)
    new_by_file = collections.defaultdict(list)
    for f in new:
        new_by_file[file_of(f)].append(f)

    matched_old = set()
    persisted, added, sevchg = [], [], []
    for nf in new:
        m = match(nf, old_by_file, a.jaccard)
        if m is not None:
            matched_old.add(id(m))
            persisted.append((nf, m))
            if sev(nf) != sev(m):
                sevchg.append((m, nf))
        else:
            added.append(nf)
    removed = [o for o in old if id(o) not in matched_old]

    def bysev(items, pick=lambda x: x):
        c = collections.Counter(sev(pick(x)) for x in items)
        return " ".join(f"{s}={c[s]}" for s in SEV if c[s])

    out = [
        f"# super-qa diff\n",
        f"- **old:** `{a.old}` ({len(old)} findings)",
        f"- **new:** `{a.new}` ({len(new)} findings)",
        f"- match: same file + title-token Jaccard >= {a.jaccard} (heuristic)\n",
        "## Summary\n",
        f"| bucket | count | by severity |",
        "| --- | ---: | --- |",
        f"| added (new) | {len(added)} | {bysev(added)} |",
        f"| removed (fixed/gone) | {len(removed)} | {bysev(removed)} |",
        f"| persisted | {len(persisted)} | {bysev(persisted, lambda x: x[0])} |",
        f"| severity changed | {len(sevchg)} | |\n",
    ]

    out.append("## Added (newly surfaced)\n")
    for f in sorted(added, key=lambda f: SEV.index(sev(f)))[:80]:
        out.append(f"- **{sev(f)}** `{f.get('location', '')}` — {f.get('title', '')}")
    out.append("\n## Removed (fixed or no longer detected)\n")
    for f in sorted(removed, key=lambda f: SEV.index(sev(f)))[:80]:
        out.append(f"- **{sev(f)}** `{f.get('location', '')}` — {f.get('title', '')}")
    if sevchg:
        out.append("\n## Severity changed\n")
        for o, n in sevchg:
            out.append(
                f"- `{n.get('location', '')}` {sev(o)} -> **{sev(n)}** — {n.get('title', '')}"
            )

    text = "\n".join(out)
    print(text)
    if a.out:
        open(a.out, "w").write(text)
        print(f"\n[wrote {a.out}]", file=sys.stderr)


if __name__ == "__main__":
    main()
