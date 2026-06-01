#!/usr/bin/env bash
# Task 4 Step 2 — rollback from backups/latest/ (defensive + idempotent).
# Restores the label taxonomy + every open issue's label set to the pre-run snapshot, detaches/re-attaches
# epic sub-issue links recorded during the run, and lists run-created projects for MANUAL deletion.
#
# Usage:
#   restore.sh                # full rollback (labels + issues + epic links)
#   restore.sh --labels-only  # labels only (used by the Task 4 Step 3 smoke-test)
source "$(dirname "$0")/lib.sh"
HERE="$(dirname "$0")"
BK="$HERE/backups/latest"
ST="$HERE/state.json"
LABELS_ONLY=0
[ "${1:-}" = "--labels-only" ] && LABELS_ONLY=1

[ -d "$BK" ] || {
	echo "ABORT: no backup at $BK — run 00-backup.sh first"
	exit 1
}
[ -f "$BK/labels.json" ] || {
	echo "ABORT: $BK/labels.json missing"
	exit 1
}

echo "== restoring label taxonomy from $BK/labels.json =="
# Converge to the snapshot in BOTH directions:
#   (a) recreate-or-update every label present in the snapshot (--force upserts; idempotent),
#   (b) delete any live label NOT in the snapshot (undoes run-created renames/additions).
# A node-stable rename in 01-sync-labels.sh (old->new) leaves the NEW name live; the new name is absent
# from the snapshot so (b) deletes it, and (a) recreates the OLD name — net effect of an undo.
jq -c '.[]' "$BK/labels.json" | while read -r l; do
	name=$(jq -r '.name' <<<"$l")
	color=$(jq -r '.color // ""' <<<"$l")
	desc=$(jq -r '.description // ""' <<<"$l")
	gh_throttle
	gh label create "$name" -R "$SLUG" --color "$color" --description "$desc" --force
done

# Delete live labels that are not in the snapshot (defensive: tolerate per-label failures).
comm -23 \
	<(gh label list -R "$SLUG" --limit 300 --json name --jq '.[].name' | sort -u) \
	<(jq -r '.[].name' "$BK/labels.json" | sort -u) | while IFS= read -r extra; do
	[ -z "$extra" ] && continue
	gh_throttle
	gh label delete "$extra" -R "$SLUG" --yes || true
	echo "deleted run-created label: $extra"
done

if [ "$LABELS_ONLY" -eq 1 ]; then
	echo "== --labels-only: done =="
	exit 0
fi

echo "== resetting open-issue label sets from $BK/open-issues.json =="
# For each open issue in the snapshot, converge its live label set back to the snapshot set:
#   remove labels present-now-but-not-then, add labels then-but-not-now.
jq -r '.[].number' "$BK/open-issues.json" | while read -r n; do
	want=$(jq -r --argjson n "$n" '.[]|select(.number==$n)|[.labels[].name]|join("\n")' "$BK/open-issues.json")
	have=$(gh issue view "$n" -R "$SLUG" --json labels --jq '.labels[].name' 2>/dev/null || true)
	args=()
	# labels to remove (have but not want)
	while IFS= read -r l; do
		[ -z "$l" ] && continue
		grep -qxF "$l" <<<"$want" || args+=(--remove-label "$l")
	done <<<"$have"
	# labels to add (want but not have)
	while IFS= read -r l; do
		[ -z "$l" ] && continue
		grep -qxF "$l" <<<"$have" || args+=(--add-label "$l")
	done <<<"$want"
	[ "${#args[@]}" -eq 0 ] && continue
	gh_throttle
	gh issue edit "$n" -R "$SLUG" "${args[@]}" || true
done

echo "== detaching/re-attaching epic sub-issue links from $ST (.epic_links[]) =="
# Each entry is {child, priorParent}. Detach the child from its CURRENT parent, then re-attach to
# priorParent when non-empty (its original parent before the run; correct under replaceParent:true).
if [ -f "$ST" ] && [ "$(jq -r '.epic_links // [] | length' "$ST" 2>/dev/null || echo 0)" -gt 0 ]; then
	jq -c '.epic_links[]' "$ST" | while read -r e; do
		child=$(jq -r '.child' <<<"$e")
		prior=$(jq -r '.priorParent // ""' <<<"$e")
		[ -z "$child" ] && continue
		cur=$(gql 'query($c:ID!){node(id:$c){... on Issue{parent{id}}}}' \
			"$(jq -nc --arg c "$child" '{c:$c}')" --jq '.data.node.parent.id // ""' 2>/dev/null || true)
		if [ -n "$cur" ]; then
			gql 'mutation($p:ID!,$c:ID!){removeSubIssue(input:{issueId:$p,subIssueId:$c}){issue{number}}}' \
				"$(jq -nc --arg p "$cur" --arg c "$child" '{p:$p,c:$c}')" >/dev/null || true
		fi
		if [ -n "$prior" ]; then
			gql 'mutation($p:ID!,$c:ID!){addSubIssue(input:{issueId:$p,subIssueId:$c,replaceParent:true}){issue{number}}}' \
				"$(jq -nc --arg p "$prior" --arg c "$child" '{p:$p,c:$c}')" >/dev/null || true
			echo "re-attached child $child -> $prior"
		else
			echo "detached child $child (no prior parent)"
		fi
	done
else
	echo "no .epic_links[] recorded — nothing to detach"
fi

echo "== run-created projects (delete MANUALLY — restore does not auto-delete projects) =="
# Diff current user projects against the pre-run snapshot; anything new was created by this run.
if [ -f "$BK/projects-before.json" ]; then
	now=$(gql 'query($l:String!){user(login:$l){projectsV2(first:50){nodes{number id title}}}}' \
		"$(jq -nc --arg l "$PM_OWNER" '{l:$l}')" --jq '.data.user.projectsV2.nodes' 2>/dev/null || echo '[]')
	jq -n --argjson now "$now" --slurpfile before "$BK/projects-before.json" \
		'($before[0] // []) as $b | $now - $b | .[] | "  manual: gh project delete \(.number) --owner '"$PM_OWNER"'  (\(.title))"' -r ||
		echo "  (could not diff projects)"
else
	echo "  (no projects-before.json snapshot)"
fi

echo "== restore complete =="
