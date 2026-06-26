#!/usr/bin/env bash
source "$(dirname "$0")/lib.sh"
MD="$(dirname "$0")/manifests"
ST="$(dirname "$0")/state.json"
# Load project node IDs from the committed lock (numbers+ids) and the regenerable state cache (field/option ids).
LOCK="$MD/projects.lock.json"
[ -f "$LOCK" ] || {
	echo "ABORT: $LOCK missing (run 05-projects-fields.sh first)"
	exit 1
}
[ -f "$ST" ] || {
	echo "ABORT: $ST missing (run 05-projects-fields.sh first)"
	exit 1
}
MI=$(jq -r '.main.id' "$LOCK")
MN=$(jq -r '.main.number' "$LOCK")
TI=$(jq -r '.triage.id' "$LOCK")
TN=$(jq -r '.triage.number' "$LOCK")
RI=$(jq -r '.roadmap.id' "$LOCK")
RN=$(jq -r '.roadmap.number' "$LOCK")
OI="$MD/open-issues.json"
[ -f "$OI" ] || {
	echo "ABORT: $OI missing (run Task 6 Step 1 fixture refresh)"
	exit 1
}

# Roadmap set: the 11 critical-path issues + #221 umbrella. The 9 epic umbrellas are created+added by 07-epics.sh
# (idempotent), so they are NOT projected here — most don't exist as issues yet at this point.
ROADMAP_SET=" 49 158 57 225 50 51 52 164 165 166 226 221 "
in_roadmap() { case "$ROADMAP_SET" in *" $1 "*) return 0 ;; *) return 1 ;; esac }
# Triage set: any open issue carrying type:bug OR type:security (read from the cached fixture labels).
is_triage() { jq -e --argjson n "$1" '.[]|select(.number==$n)|[.labels[].name]|any(.=="type:bug" or .=="type:security")' "$OI" >/dev/null 2>&1; }

# set_field: project_id item_id field_id option_id  (last-write-wins ⇒ idempotent; Task 11 Step 2)
set_field() {
	gql 'mutation($p:ID!,$i:ID!,$f:ID!,$o:String!){updateProjectV2ItemFieldValue(input:{projectId:$p,itemId:$i,fieldId:$f,value:{singleSelectOptionId:$o}}){projectV2Item{id}}}' "$(jq -nc --arg p "$1" --arg i "$2" --arg f "$3" --arg o "$4" '{p:$p,i:$i,f:$f,o:$o}')" >/dev/null
}

# --- Step 1: add items, capturing the returned item id into per-project <project>-items.tsv maps ---
echo "== adding items to Main (all open) =="
jq -r '.[]|"\(.number)\t\(.id)"' "$OI" | while IFS=$'\t' read -r n id; do
	iid=$(project_item_add "$MI" "$id")
	printf '%s\t%s\n' "$n" "$iid"
done >"$MD/main-items.tsv"

echo "== adding items to Triage (type:bug ∨ type:security) =="
jq -r '.[]|"\(.number)\t\(.id)"' "$OI" | while IFS=$'\t' read -r n id; do
	is_triage "$n" || continue
	iid=$(project_item_add "$TI" "$id")
	printf '%s\t%s\n' "$n" "$iid"
done >"$MD/triage-items.tsv"

echo "== adding items to Roadmap (11 critical-path + #221) =="
jq -r '.[]|"\(.number)\t\(.id)"' "$OI" | while IFS=$'\t' read -r n id; do
	in_roadmap "$n" || continue
	iid=$(project_item_add "$RI" "$id")
	printf '%s\t%s\n' "$n" "$iid"
done >"$MD/roadmap-items.tsv"

# --- Step 2: set Priority/Phase per issue-map.tsv ---
# Phase field exists only on Main + Roadmap; Priority exists on all three (see projects.json).
MAP="$MD/issue-map.tsv"
[ -f "$MAP" ] || {
	echo "ABORT: $MAP missing"
	exit 1
}

item_id() { awk -F'\t' -v n="$2" '$1==n{print $2; exit}' "$1"; }                        # tsv n -> item id
opt_id() { jq -r --arg v "$3" ".${1}.fields.\"${2}\".options[\$v] // \"null\"" "$ST"; } # project field value -> option id
fld_id() { jq -r ".${1}.fields.\"${2}\".id // \"null\"" "$ST"; }                        # project field -> field id

# apply <project> <items-tsv> <project_id> <issue> <Field> <value>
apply() {
	local proj="$1" tsv="$2" pid="$3" n="$4" field="$5" val="$6" iid fid oid
	[ -n "$val" ] || return 0 # empty = legitimately unset → skip
	iid=$(item_id "$tsv" "$n")
	[ -n "$iid" ] || return 0 # issue not in this project → skip
	fid=$(fld_id "$proj" "$field")
	[ "$fid" = null ] && {
		echo "ABORT #$n $proj.$field: field id null"
		exit 1
	}
	oid=$(opt_id "$proj" "$field" "$val")
	[ "$oid" = null ] && {
		echo "ABORT #$n $proj.$field: option '$val' resolves to null (value-name typo?)"
		exit 1
	}
	set_field "$pid" "$iid" "$fid" "$oid"
}

echo "== setting Priority/Phase fields =="
tail -n +2 "$MAP" | while IFS= read -r line; do
	# Parse TSV fields with `cut` (preserves EMPTY phase/prio): `IFS=$'\t' read` collapses consecutive
	# tabs because tab is IFS-whitespace, which would shift `reason` into `phase` on rows with empty cols.
	n=$(cut -f1 <<<"$line")
	phase=$(cut -f4 <<<"$line")
	prio=$(cut -f5 <<<"$line")
	# Priority: Main always; Triage iff bug/security; Roadmap iff in roadmap set.
	apply main "$MD/main-items.tsv" "$MI" "$n" Priority "$prio"
	is_triage "$n" && apply triage "$MD/triage-items.tsv" "$TI" "$n" Priority "$prio"
	in_roadmap "$n" && apply roadmap "$MD/roadmap-items.tsv" "$RI" "$n" Priority "$prio"
	# Phase: Main always; Roadmap iff in roadmap set. (Triage has no Phase field.)
	apply main "$MD/main-items.tsv" "$MI" "$n" Phase "$phase"
	in_roadmap "$n" && apply roadmap "$MD/roadmap-items.tsv" "$RI" "$n" Phase "$phase"
	: # keep loop-body exit 0 — a trailing `&&` can short-circuit to 1 on a non-roadmap row (pipefail would abort)
done
echo "done."
