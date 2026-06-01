#!/usr/bin/env bash
source "$(dirname "$0")/lib.sh"
MD="$(dirname "$0")/manifests"
ST="$(dirname "$0")/state.json"
# epics.json schema (Task 12 Step 1): { "epics": [ {title, area, state, existing_issue?, members:[<number>...]} ] }
# Single-parent is GUARANTEED by the manifest (Task 12 Step 3 uniq -d guard). Members are issue NUMBERS.
EP="$MD/epics.json"
LOCK="$MD/projects.lock.json"
OI="$MD/open-issues.json"
for f in "$EP" "$LOCK" "$OI"; do [ -f "$f" ] || {
	echo "ABORT: $f missing"
	exit 1
}; done
MI=$(jq -r '.main.id' "$LOCK")
RI=$(jq -r '.roadmap.id' "$LOCK")
[ -f "$ST" ] || echo '{}' >"$ST" # ensure state.json exists for epic_links accumulation

# Open-issue set (numbers) for membership filtering — INV-OPEN-ONLY.
is_open() { jq -e --argjson n "$1" 'any(.[]; .number==$n)' "$OI" >/dev/null 2>&1; }

# Append a rollback entry {child, priorParent} to state.json.epic_links[] (atomic; only on a real change).
record_link() {
	local child="$1" prior="$2" tmp="$ST.tmp"
	jq --arg c "$child" --arg p "$prior" '.epic_links = ((.epic_links // []) + [{child:$c, priorParent:$p}])' "$ST" >"$tmp" && mv "$tmp" "$ST"
}

jq -c '.epics[]' "$EP" | while read -r e; do
	title=$(jq -r '.title' <<<"$e")
	area=$(jq -r '.area // ""' <<<"$e")
	ex=$(jq -r '.existing_issue // ""' <<<"$e")
	echo "== epic: $title =="

	# Resolve-or-create the umbrella issue → epic number + node id.
	if [ -n "$ex" ] && [ "$ex" != null ]; then
		enum="$ex"
	else
		labels=(--label type:epic)
		[ -n "$area" ] && [ "$area" != null ] && labels+=(--label "$area")
		body="Umbrella tracking issue. Sub-issues are linked below."
		gh_ratecheck
		gh_throttle
		url=$(gh issue create -R "$SLUG" --title "$title" --body "$body" "${labels[@]}")
		enum=$(grep -oE '[0-9]+$' <<<"$url")
		[ -n "$enum" ] || {
			echo "ABORT: could not parse issue number from '$url'"
			exit 1
		}
	fi
	enode=$(issue_node "$enum")
	[ -n "$enode" ] || {
		echo "ABORT: no node id for epic #$enum"
		exit 1
	}

	# Add the umbrella to Main + Roadmap (idempotent via paginated project_item_add).
	project_item_add "$MI" "$enode" >/dev/null
	project_item_add "$RI" "$enode" >/dev/null

	# Link each OPEN member as a sub-issue (replaceParent:true ⇒ idempotent single-parent re-parent).
	jq -r '.members[]?' <<<"$e" | while read -r m; do
		[ -n "$m" ] || continue
		if ! is_open "$m"; then
			echo "skip member #$m (not in open set) — INV-OPEN-ONLY"
			continue
		fi
		cnode=$(issue_node "$m")
		[ -n "$cnode" ] || {
			echo "ABORT: no node id for member #$m"
			exit 1
		}
		res=$(subissue_link "$enode" "$cnode") # "skip" | "changed <priorParentId-or-empty>"
		case "$res" in
		skip) echo "  #$m already under #$enum (no-op)" ;;
		changed*)
			prior="${res#changed}"
			prior="${prior# }"
			record_link "$cnode" "$prior"
			echo "  linked #$m -> #$enum (prior='$prior')"
			;;
		*)
			echo "ABORT: unexpected subissue_link result '$res' for #$m"
			exit 1
			;;
		esac
	done
done
echo "done."
