#!/usr/bin/env bash
source "$(dirname "$0")/lib.sh"
MD="$(dirname "$0")/manifests"
LOCK="$MD/projects.lock.json"
[ -f "$LOCK" ] || {
	echo "ABORT: $LOCK missing (run 05-projects-fields.sh first)"
	exit 1
}

# Resolve the action tag v1 -> immutable commit SHA (Codex C7 / Gemini G5 — SHA-pin in the shipped workflow).
SHA=$(gh api repos/actions/add-to-project/commits/v1 --jq '.sha')
[ -n "$SHA" ] || {
	echo "ABORT: could not resolve actions/add-to-project v1 SHA"
	exit 1
}

# Literal project numbers from the committed lock.
MAIN_NUM=$(jq -r '.main.number' "$LOCK")
TRIAGE_NUM=$(jq -r '.triage.number' "$LOCK")
for v in "$MAIN_NUM" "$TRIAGE_NUM"; do
	case "$v" in '' | null)
		echo "ABORT: missing project number in $LOCK"
		exit 1
		;;
	esac
done

OUT=".github/workflows/add-to-project.yml"
mkdir -p "$(dirname "$OUT")"

# Single-quoted heredoc keeps ${{ secrets... }} literal; placeholders injected via sed afterward.
cat >"$OUT" <<'EOF'
name: Auto-add to projects
on:
  issues: { types: [opened, reopened, labeled] } # 'labeled' so a later type:bug/type:security routes to Triage
  pull_request: { types: [opened, reopened] }
jobs:
  add-to-main:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/add-to-project@__SHA__ # pinned to the v1 commit SHA (supply-chain hardening)
        with:
          project-url: https://github.com/users/dutiona/projects/__MAIN_NUM__
          github-token: ${{ secrets.MEMORY_ENGINE_PROJECT_TOKEN }}
  add-to-triage:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/add-to-project@__SHA__ # pinned to the v1 commit SHA
        with:
          project-url: https://github.com/users/dutiona/projects/__TRIAGE_NUM__
          github-token: ${{ secrets.MEMORY_ENGINE_PROJECT_TOKEN }}
          labeled: type:bug, type:security
          label-operator: OR
EOF

sed -i \
	-e "s|__SHA__|${SHA}|g" \
	-e "s|__MAIN_NUM__|${MAIN_NUM}|g" \
	-e "s|__TRIAGE_NUM__|${TRIAGE_NUM}|g" \
	"$OUT"
echo "rendered $OUT (SHA=$SHA main=$MAIN_NUM triage=$TRIAGE_NUM)"
