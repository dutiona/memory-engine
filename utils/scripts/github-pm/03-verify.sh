#!/usr/bin/env bash
source "$(dirname "$0")/lib.sh"
MD="$(dirname "$0")/manifests"
MODE="${1:---type-area}"
fail=0
gh issue list -R "$SLUG" --state open --limit 500 --json number,labels | jq -e '
  [ .[] | {n:.number, t:([.labels[].name|select(startswith("type:"))]|length),
           a:([.labels[].name|select(startswith("area:"))]|length),
           epic:([.labels[].name]|index("type:epic"))}
    | select(.t!=1 or (.epic==null and .a!=1) or (.epic!=null and .a>1)) ] as $bad
  | if ($bad|length)>0 then ("FAIL \($bad|length):\n"+($bad|map("  #\(.n) types=\(.t) areas=\(.a)")|join("\n"))|halt_error(1))
    else "OK: type+area on all open issues" end' || fail=1
if [ "$MODE" = "--full" ]; then # post-delete: live label set must EQUAL the manifest
	diff <(gh label list -R "$SLUG" --limit 300 --json name --jq '[.[].name]|sort|.[]') \
		<(jq -r '.[].name' "$MD"/labels.core.json "$MD"/labels.area.memory-engine.json | sort) &&
		echo "LABELS OK" || fail=1
	for L in duplicate invalid wontfix "help wanted" "good first issue" question phase-5 phase-5a phase-5b phase-6 deferred; do
		gh label list -R "$SLUG" --json name --jq '.[].name' | grep -qx "$L" && {
			echo "FAIL: '$L' still present"
			fail=1
		}
	done
fi
exit $fail
