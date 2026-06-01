#!/usr/bin/env bash
# Task 4 Step 1 — non-destructive snapshot of the pre-migration repo state.
# Writes four artifacts under backups/<UTC-timestamp>/ and points backups/latest at it.
# Idempotent: each run creates a fresh timestamped dir; re-pointing the symlink is safe.
source "$(dirname "$0")/lib.sh"
HERE="$(dirname "$0")"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
DIR="$HERE/backups/$TS"
mkdir -p "$DIR"

# 1. Current label taxonomy (name,color,description) — restore.sh recreates/renames from this.
gh label list -R "$SLUG" --limit 300 --json name,color,description >"$DIR/labels.json"

# 2. Open issues with their full label set — restore.sh converges each open issue back to this.
gh issue list -R "$SLUG" --state open --limit 500 --json number,title,labels >"$DIR/open-issues.json"

# 3. Every issue (open + closed) with state + labels — read-only deletion-safety audit reference.
gh issue list -R "$SLUG" --state all --limit 1000 --json number,state,labels >"$DIR/all-issue-labels.json"

# 4. User-owned projects before the run (gql, per spec — never `gh project list`) so restore.sh can
#    identify run-created projects by diffing against this snapshot.
gql 'query($l:String!){user(login:$l){projectsV2(first:50){nodes{number id title}}}}' \
	"$(jq -nc --arg l "$PM_OWNER" '{l:$l}')" \
	--jq '.data.user.projectsV2.nodes' >"$DIR/projects-before.json"

# Point backups/latest at this snapshot (-n: treat existing symlink as a file, never descend into it).
ln -sfn "$TS" "$HERE/backups/latest"

echo "backup → $DIR"
ls -1 "$DIR"
