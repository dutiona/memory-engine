#!/usr/bin/env bash
source "$(dirname "$0")/lib.sh"
bash "$(dirname "$0")/03-verify.sh" --type-area || {
	echo "GATE RED — refusing to delete"
	exit 1
}
TARGETS=(duplicate invalid wontfix "help wanted" "good first issue" question design performance quality)
for L in "${TARGETS[@]}"; do
	cnt=$(gh issue list -R "$SLUG" --state all --label "$L" --limit 1000 --json number --jq 'length' 2>/dev/null || echo 0)
	[ "$cnt" -gt 0 ] && {
		echo "WARN '$L' still on $cnt issue(s) — NOT deleting"
		continue
	}
	gh_throttle
	gh label delete "$L" -R "$SLUG" --yes || true
	echo "deleted $L"
done
