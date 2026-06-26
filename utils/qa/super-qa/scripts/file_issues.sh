#!/usr/bin/env bash
# Super-QA issue filer. Idempotent, resumable, reuses scripts/github-pm/lib.sh.
# Phases: labels | epics | findings | dupes | observers | relabel | all
# Env: DRY=1 (no writes), GH_PM_THROTTLE (default bumped to 1.0s for issue creation).
set -euo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(git -C "$DIR" rev-parse --show-toplevel)"
export GH_PM_THROTTLE="${GH_PM_THROTTLE:-1.0}"
source "$ROOT/scripts/github-pm/lib.sh"
# Inputs default to the 2026-06-01 run; override RUN_DIR (or MANIFEST/STATE) for another round.
RUN_DIR="${RUN_DIR:-$DIR/../runs/2026-06-01}"
MANIFEST="${MANIFEST:-$RUN_DIR/issue_manifest.json}"
STATE="${STATE:-$RUN_DIR/issue_state.json}"
TMP="$(mktemp -d)"
DRY="${DRY:-0}"
PHASE="${1:-all}"
[ -f "$STATE" ] || echo '{}' >"$STATE"
log() { echo "[$(date +%H:%M:%S)] $*"; }
prefetch_titles() { # one fetch of all issue titles for idempotent title-match
	gh issue list -R "$SLUG" --state all --limit 2000 --json number,title >"$TMP/titles.json" 2>/dev/null || echo '[]' >"$TMP/titles.json"
	log "prefetched $(jq 'length' "$TMP/titles.json") existing issue titles"
}

st_get() { jq -r --arg k "$1" '.[$k] // ""' "$STATE"; }
st_put() {
	local tmp="$STATE.tmp"
	jq --arg k "$1" --arg v "$2" '.[$k]=$v' "$STATE" >"$tmp" && mv "$tmp" "$STATE"
}

# create_issue KEY  (reads title/body/labels from manifest by key) -> sets state, echoes number
create_from_key() {
	local key="$1"
	local ex
	ex="$(st_get "$key")"
	[ -n "$ex" ] && {
		echo "$ex"
		return 0
	}
	local title body
	title="$(jq -r --arg k "$key" '.manifest[]|select(.key==$k)|.title' "$MANIFEST")"
	jq -r --arg k "$key" '.manifest[]|select(.key==$k)|.body' "$MANIFEST" >"$TMP/body.md"
	# label args
	local labelargs=()
	while IFS= read -r l; do labelargs+=(--label "$l"); done < <(jq -r --arg k "$key" '.manifest[]|select(.key==$k)|.labels[]' "$MANIFEST")
	# title-match fallback against prefetched titles (state-loss resilience, 1 fetch not N)
	local found=""
	[ -f "$TMP/titles.json" ] && found="$(jq -r --arg t "$title" '.[]|select(.title==$t)|.number' "$TMP/titles.json" 2>/dev/null | head -1 || true)"
	[ -n "$found" ] && {
		st_put "$key" "$found"
		echo "$found"
		return 0
	}
	if [ "$DRY" = 1 ]; then
		echo "DRY create: [$key] $title  labels=$(jq -r --arg k "$key" '[.manifest[]|select(.key==$k)|.labels[]]|join(",")' "$MANIFEST")" >&2
		echo "0"
		return 0
	fi
	local url num tries=0
	while :; do
		if url="$(gh issue create -R "$SLUG" --title "$title" --body-file "$TMP/body.md" "${labelargs[@]}" 2>"$TMP/err")"; then
			break
		fi
		tries=$((tries + 1))
		if [ "$tries" -ge 5 ]; then
			log "ABORT create [$key]: $(cat "$TMP/err")"
			return 1
		fi
		log "retry $tries [$key] ($(head -1 "$TMP/err"))"
		sleep $((tries * 15))
	done
	num="$(grep -oE '[0-9]+$' <<<"$url")"
	st_put "$key" "$num"
	gh_throttle
	echo "$num"
}

link_child() { # parent_key child_num
	local pnum
	pnum="$(st_get "$1")"
	[ -n "$pnum" ] || {
		log "WARN no parent for $1"
		return 0
	}
	[ "$2" = 0 ] && return 0
	if [ "$DRY" = 1 ]; then
		echo "DRY link #$2 -> $1(#$pnum)" >&2
		return 0
	fi
	local pnode cnode
	pnode="$(issue_node "$pnum")" || return 0
	cnode="$(issue_node "$2")" || return 0
	subissue_link "$pnode" "$cnode" >/dev/null 2>&1 || log "WARN link #$2->#$pnum failed"
}

phase_labels() {
	log "PHASE labels: ensure severity:blocker"
	[ "$DRY" = 1 ] && {
		echo "DRY label severity:blocker"
		return 0
	}
	label_upsert "severity:blocker" "8B0000" "super-qa severity: blocker (build-breaking)" || true
}

phase_epics() {
	log "PHASE epics"
	prefetch_titles
	local m a
	m="$(create_from_key epic:master)"
	log "master=#$m"
	create_from_key epic:autofix >/dev/null
	link_child epic:master "$(st_get epic:autofix)"
	# area epics
	while IFS= read -r k; do
		create_from_key "$k" >/dev/null
		link_child epic:master "$(st_get "$k")"
		log "area epic $k = #$(st_get "$k")"
	done < <(jq -r '.manifest[]|select(.kind=="area-epic")|.key' "$MANIFEST")
	# link prior-run epic #241 under master
	link_child epic:master 241
}

phase_findings() {
	log "PHASE findings/li-groups/autofix"
	prefetch_titles
	local k parent n
	for kind in finding li-group autofix; do
		while IFS= read -r k; do
			n="$(create_from_key "$k")"
			parent="$(jq -r --arg k "$k" '.manifest[]|select(.key==$k)|.parent' "$MANIFEST")"
			link_child "$parent" "$n"
		done < <(jq -r --arg kd "$kind" '.manifest[]|select(.kind==$kd)|.key' "$MANIFEST")
		log "  done kind=$kind"
	done
}

phase_dupes() {
	log "PHASE dupes: link existing issues into area epics"
	while IFS= read -r row; do
		local num pkey
		num="${row%%=*}"
		pkey="${row#*=}"
		link_child "$pkey" "$num"
		log "  linked existing #$num -> $pkey"
	done < <(jq -r '.links_existing|to_entries[]|"\(.key)=\(.value)"' "$MANIFEST")
}

phase_observers() {
	log "PHASE observers (patch bodies with #lists)"
	for s in critical high medium low info; do
		local key="observer:$s"
		jq -e --arg k "$key" '.manifest[]|select(.key==$k)' "$MANIFEST" >/dev/null 2>&1 || continue
		# build #-list from state for non-autofix findings of severity s
		local list="" fk fn
		while IFS= read -r fk; do
			fn="$(st_get "$fk")"
			[ -n "$fn" ] && [ "$fn" != 0 ] && list+="- #$fn"$'\n'
		done < <(jq -r --arg s "$s" '.manifest[]|select(.kind=="finding" and .severity==$s)|.key' "$MANIFEST")
		jq -r --arg k "$key" '.manifest[]|select(.key==$k)|.body' "$MANIFEST" >"$TMP/obody.md"
		printf '\n## Issues\n%s\n' "$list" >>"$TMP/obody.md"
		local ex
		ex="$(st_get "$key")"
		if [ -n "$ex" ]; then
			[ "$DRY" = 1 ] || gh issue edit "$ex" -R "$SLUG" --body-file "$TMP/obody.md" >/dev/null
		else
			local title labelargs=()
			title="$(jq -r --arg k "$key" '.manifest[]|select(.key==$k)|.title' "$MANIFEST")"
			while IFS= read -r l; do labelargs+=(--label "$l"); done < <(jq -r --arg k "$key" '.manifest[]|select(.key==$k)|.labels[]' "$MANIFEST")
			if [ "$DRY" = 1 ]; then
				echo "DRY observer $key"
			else
				local url
				url="$(gh issue create -R "$SLUG" --title "$title" --body-file "$TMP/obody.md" "${labelargs[@]}")"
				st_put "$key" "$(grep -oE '[0-9]+$' <<<"$url")"
				gh_throttle
			fi
		fi
		link_child epic:master "$(st_get "$key")"
		log "  observer $s = #$(st_get "$key")"
	done
}

# relabel existing 23 to new scheme: severity from title, ensure super-qa, keep area/type
phase_relabel() {
	log "PHASE relabel existing super-qa issues"
	gh issue list -R "$SLUG" --label super-qa --state open --limit 300 --json number,title,labels >"$TMP/ex.json"
	jq -c '.[]' "$TMP/ex.json" | while read -r row; do
		local num title sev
		num="$(jq -r '.number' <<<"$row")"
		title="$(jq -r '.title' <<<"$row")"
		sev="$(grep -oiE '\[super-qa\] (blocker|critical|high|medium|low|info)' <<<"$title" | grep -oiE '(blocker|critical|high|medium|low|info)' | head -1 | tr 'A-Z' 'a-z' || true)"
		# default low for the "low findings"/"info findings" bucket issues
		[ -z "$sev" ] && grep -qiE 'low findings' <<<"$title" && sev=low
		[ -z "$sev" ] && grep -qiE 'info findings' <<<"$title" && sev=info
		[ -z "$sev" ] && continue
		jq -e --arg l "severity:$sev" '.labels[]|select(.name==$l)' <<<"$row" >/dev/null 2>&1 && continue
		if [ "$DRY" = 1 ]; then
			echo "DRY relabel #$num += severity:$sev"
		else
			gh issue edit "$num" -R "$SLUG" --add-label "severity:$sev" >/dev/null && gh_throttle
			log "  #$num += severity:$sev"
		fi
	done
}

case "$PHASE" in
labels) phase_labels ;;
epics) phase_epics ;;
findings) phase_findings ;;
dupes) phase_dupes ;;
observers) phase_observers ;;
relabel) phase_relabel ;;
all)
	phase_labels
	phase_epics
	phase_findings
	phase_dupes
	phase_observers
	phase_relabel
	;;
*)
	echo "unknown phase: $PHASE"
	exit 1
	;;
esac
log "PHASE '$PHASE' complete."
