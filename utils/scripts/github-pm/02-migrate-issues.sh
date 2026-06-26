#!/usr/bin/env bash
source "$(dirname "$0")/lib.sh"
MAP="$(dirname "$0")/manifests/issue-map.tsv"
grep -qP '\tAMBIGUOUS(\t|$)' "$MAP" && {
	echo "ABORT: AMBIGUOUS rows remain"
	exit 1
}
tail -n +2 "$MAP" | while IFS=$'\t' read -r n type area phase prio epic reason; do
	case "$type" in type:*) ;; *)
		echo "ABORT #$n bad type '$type'"
		exit 1
		;;
	esac
	[ "$type" = "type:epic" ] || case "$area" in area:*) ;; *)
		echo "ABORT #$n bad area '$area'"
		exit 1
		;;
	esac
	# Reconcile to EXACTLY {type, area} (fixes Gemini G2 + idempotency G4/G7): strip ANY other type:*/area:*
	# the issue carries — including a rename-derived type that differs from the mapped one (the canonical
	# case: a Plan: issue renamed documentation->type:docs but mapped to type:plan). Not just type:enhancement.
	rm=()
	# Read current labels from the cached fixture (Gemini: avoids a `gh issue view` network call per issue) +
	# `while read` (no word-splitting). open-issues.json is the post-Task-5 live snapshot from Task 6 Step 1.
	while read -r l; do
		case "$l" in
		type:*) [ "$l" = "$type" ] || rm+=(--remove-label "$l") ;;
		area:*) { [ "$type" = "type:epic" ] || [ "$l" = "$area" ]; } || rm+=(--remove-label "$l") ;;
		esac
	done < <(jq -r --argjson n "$n" '.[]|select(.number==$n)|.labels[].name' "$(dirname "$0")/manifests/open-issues.json")
	rm+=(--remove-label design --remove-label performance --remove-label quality)
	add=(--add-label "$type")
	[ "$type" = "type:epic" ] || add+=(--add-label "$area")
	gh_ratecheck
	gh_throttle
	gh issue edit "$n" -R "$SLUG" "${add[@]}" "${rm[@]}"
done
