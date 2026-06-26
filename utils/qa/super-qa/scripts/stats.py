#!/usr/bin/env python3
"""Stats over a super-qa consolidated findings JSON.

Usage:
    python3 stats.py <consolidated.json> [--out stats.md]

Prints (and optionally writes) severity / category / module / area tables,
auto-fixable + verified + security-fallback counts. Works on any run's
consolidated.json so successive rounds are directly comparable.
"""

import json, sys, collections, argparse

SEV = ["blocker", "critical", "high", "medium", "low", "info"]
MOD2AREA = {
    "engine": "core",
    "core-root": "core",
    "pool": "core",
    "resume": "core",
    "embed": "core",
    "scope": "core",
    "bootstrap": "core",
    "store": "storage",
    "archive": "storage",
    "inspect": "storage",
    "search": "retrieval",
    "graph": "retrieval",
    "consolidation": "consolidation",
    "forgetting": "forgetting",
    "conflict": "temporal",
    "cli": "cli",
    "mcp": "mcp",
    "workspace": "build",
    "sc": "build",
}


def sev(f):
    s = str(f.get("severity", "info")).lower()
    return s if s in SEV else "info"


def module(f):
    i = str(f.get("id", "") or "")
    return i.split("/")[0] if "/" in i else "workspace"


def area(f):
    if (
        "cognitive" in str(f.get("location", "")).lower()
        or "dream" in str(f.get("title", "")).lower()
    ):
        return "cognitive"
    return MOD2AREA.get(module(f), "core")


def cat(f):
    return str(f.get("category_norm", f.get("category", "?")) or "?").lower()


def table(title, counter, extra=None):
    lines = [
        f"### {title}\n",
        "| key | count |" + (" | auto-fix |" if extra else ""),
        "| --- | ---: |" + (" ---: |" if extra else ""),
    ]
    for k, n in counter.most_common():
        row = f"| {k} | {n} |"
        if extra is not None:
            row += f" {extra.get(k, 0)} |"
        lines.append(row)
    return "\n".join(lines) + "\n"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("consolidated")
    ap.add_argument("--out")
    a = ap.parse_args()
    ded = json.load(open(a.consolidated))
    if isinstance(ded, dict) and "findings" in ded:
        ded = ded["findings"]

    out = [
        f"# super-qa stats — {a.consolidated}\n",
        f"**Total findings:** {len(ded)}\n",
    ]

    # severity x auto-fixable
    bysev = collections.Counter(sev(f) for f in ded)
    afix = collections.Counter(sev(f) for f in ded if f.get("auto_fixable"))
    out.append(
        "### Severity\n\n| severity | count | auto-fixable | report-only |\n| --- | ---: | ---: | ---: |"
    )
    for s in SEV:
        n = bysev.get(s, 0)
        af = afix.get(s, 0)
        out.append(f"| {s} | {n} | {af} | {n - af} |")
    out.append(
        f"| **total** | **{len(ded)}** | **{sum(afix.values())}** | **{len(ded) - sum(afix.values())}** |\n"
    )

    out.append(table("Category (normalized)", collections.Counter(cat(f) for f in ded)))
    out.append(table("Module", collections.Counter(module(f) for f in ded)))
    out.append(table("Area", collections.Counter(area(f) for f in ded)))

    verified = sum(1 for f in ded if f.get("_verified"))
    fallback = sum(1 for f in ded if f.get("fallback_used"))
    out.append(
        f"### Provenance\n\n- source/compiler-verified: **{verified}**\n"
        f"- security-fallback (octo tier-2): **{fallback}**\n"
        f"- auto-fixable: **{sum(afix.values())}**\n"
    )

    text = "\n".join(out)
    print(text)
    if a.out:
        open(a.out, "w").write(text)
        print(f"\n[wrote {a.out}]", file=sys.stderr)


if __name__ == "__main__":
    main()
