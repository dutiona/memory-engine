#!/usr/bin/env bash
source "$(dirname "$0")/lib.sh"
F="$(dirname "$0")/manifests/open-issues.json"
printf 'issue\ttype\tarea\tphase\tpriority\tepic\treason\n'
jq -r '.[] | [.number, .title, ([.labels[].name]|join(","))] | @tsv' "$F" | while IFS=$'\t' read -r n title labels; do
	t=""
	reason=""
	ct=$(printf '%s' "$labels" | tr ',' '\n' | grep -m1 '^type:' || true) # canonical type:* from a Task-5 rename
	# 1. Title overrides (BOTH legacy `Plan:` and conventional `plan(scope):`/`plan:`; same for umbrellas).
	#    Fixes Codex C8: the plan-issue title is `plan(build): ...`, not `Plan:`.
	case "$title" in
	"Plan:"* | "Plan archive:"* | plan\(* | "plan:"*)
		t="type:plan"
		reason="title-plan"
		;;
	"Umbrella:"* | *[Uu]mbrella*)
		t="type:epic"
		reason="title-umbrella"
		;;
	esac
	# 2. Existing canonical type:* (authoritative post-rename — fixes Gemini G1; enhancement falls through to split).
	if [ -z "$t" ]; then case "$ct" in type:enhancement) ;; type:*)
		t="$ct"
		reason="existing-type"
		;;
	esac fi
	# 3. Legacy (pre-rename) names — in case the generator is run before Task 5.
	if [ -z "$t" ]; then case ",$labels," in
		*,bug,*)
			t="type:bug"
			reason="label-bug"
			;;
		*,security,*)
			t="type:security"
			reason="label-security"
			;;
		*,testing,*)
			t="type:test"
			reason="label-test"
			;;
		*,documentation,*)
			t="type:docs"
			reason="label-docs"
			;;
		*,refactoring,* | *,quality,* | *,performance,*)
			t="type:refactor"
			reason="label-refactor"
			;;
		*,research,* | *,design,*)
			t="type:research"
			reason="label-research/design"
			;;
		esac fi
	# 4. enhancement split + brand-new issues: conventional title prefix; keep enhancement if no signal; else AMBIGUOUS.
	if [ -z "$t" ] || [ "$ct" = type:enhancement ]; then case "$title" in
		feat\(* | "feat:"*)
			t="type:feature"
			reason="title-feat"
			;;
		fix\(* | "fix:"*)
			t="type:bug"
			reason="title-fix"
			;;
		refactor\(* | perf*)
			t="type:refactor"
			reason="title-refactor"
			;;
		*) [ "$ct" = type:enhancement ] && {
			t="type:enhancement"
			reason="keep-enhancement"
		} ;;
		esac fi
	[ -z "$t" ] && {
		t="AMBIGUOUS"
		reason="needs-human"
	}
	a=$(printf '%s' "$title" | sed -nE 's/^[a-z]+\(([a-z]+)\):.*/\1/p') # area from title type(area): hint
	case "$a" in core | storage | retrieval | consolidation | forgetting | temporal | cognitive | knowledge | cli | mcp | docs | build | qa | viz) area="area:$a" ;; *) area="AMBIGUOUS" ;; esac
	phase=""
	case ",$labels," in *,phase-5a,*) phase="Phase 5a" ;; *,phase-5b,*) phase="Phase 5b" ;; *,phase-5,*) phase="Phase 5 (indep)" ;; *,phase-6,*) phase="Phase 6" ;; *,deferred,*) phase="Deferred" ;; esac
	# Explicit ROADMAP placements not carried by a phase-* label (Codex-connector P2): snapshot follow-ups + #13.
	[ -z "$phase" ] && case " $n " in " 199 " | " 200 " | " 201 " | " 203 " | " 204 " | " 205 ") phase="Phase 4" ;; " 13 ") phase="Phase 7" ;; esac
	# priority: ROADMAP critical path -> P0 (the two immediate unblockers) / P1 (rest of the 11 + #221); else empty (reviewer fills at the gate).
	prio=""
	case " $n " in " 49 " | " 158 ") prio="P0 Critical" ;; " 57 " | " 225 " | " 50 " | " 51 " | " 52 " | " 164 " | " 165 " | " 166 " | " 226 " | " 221 ") prio="P1 High" ;; esac
	printf '%s\t%s\t%s\t%s\t%s\t\t%s\n' "$n" "$t" "$area" "$phase" "$prio" "$reason"
done
