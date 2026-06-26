#!/usr/bin/env bash
# Task 16 + 17 — retire the phase-* / deferred labels once their semantics live in the Main project's
# Phase field. Gate (Task 16): every open issue carrying a phase-*/deferred label MUST have a non-empty
# Phase value on its Main project item; abort listing gaps if any. Then delete the 5 labels and re-run
# the full verifier. Idempotent: re-running after retirement finds no labelled issues and no labels.
source "$(dirname "$0")/lib.sh"
HERE="$(dirname "$0")"
ST="$HERE/state.json"
PHASE_LABELS=(phase-5 phase-5a phase-5b phase-6 deferred)

[ -f "$ST" ] || {
	echo "ABORT: $ST missing — run 05-projects-fields.sh first"
	exit 1
}
MAIN_PID=$(jq -r '.main.id // ""' "$ST")
PHASE_FID=$(jq -r '.main.fields.Phase.id // ""' "$ST")
[ -n "$MAIN_PID" ] || {
	echo "ABORT: .main.id absent in $ST"
	exit 1
}
[ -n "$PHASE_FID" ] || {
	echo "ABORT: .main.fields.Phase.id absent in $ST"
	exit 1
}

# Build the OR label filter for one query of every open issue carrying any retiring label.
echo "== Task 16: asserting Phase-field coverage on Main items =="
labelled=$(printf '%s\n' "${PHASE_LABELS[@]}" | while read -r L; do
	gh issue list -R "$SLUG" --state open --label "$L" --limit 1000 --json number --jq '.[].number'
done | sort -un)

gaps=0
for n in $labelled; do
	[ -z "$n" ] && continue
	# Resolve the issue's node id, then read the Phase single-select value on its Main project item.
	nid=$(issue_node "$n")
	val=$(gql 'query($c:ID!){node(id:$c){... on Issue{projectItems(first:50){nodes{
            project{id}
            phase:fieldValueByName(name:"Phase"){... on ProjectV2ItemFieldSingleSelectValue{name}}
          }}}}}' \
		"$(jq -nc --arg c "$nid" '{c:$c}')" \
		--jq "[.data.node.projectItems.nodes[]|select(.project.id==\"$MAIN_PID\")|.phase.name//empty]|first // \"\"" 2>/dev/null || echo "")
	if [ -z "$val" ]; then
		echo "  GAP #$n — labelled phase-*/deferred but no Phase value on its Main item"
		gaps=$((gaps + 1))
	fi
done

if [ "$gaps" -gt 0 ]; then
	echo "ABORT: $gaps issue(s) lack a Main Phase value — back-fill before retiring labels (Task 16 must pass)"
	exit 1
fi
echo "Phase coverage OK (no gaps)"

echo "== Task 17: deleting phase-*/deferred labels =="
for L in "${PHASE_LABELS[@]}"; do
	gh label list -R "$SLUG" --json name --jq '.[].name' | grep -qx "$L" || {
		echo "skip delete $L (absent)"
		continue
	}
	gh_throttle
	gh label delete "$L" -R "$SLUG" --yes || true
	echo "deleted $L"
done

echo "== re-running 03-verify.sh --full =="
bash "$HERE/03-verify.sh" --full
