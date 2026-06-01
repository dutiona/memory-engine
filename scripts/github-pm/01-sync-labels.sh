#!/usr/bin/env bash
source "$(dirname "$0")/lib.sh"
MD="$(dirname "$0")/manifests"
# 1. Renames FIRST — node-id-stable (INV-RENAME). enhancement→type:enhancement keeps all 46 assocs;
#    the SPLIT to type:feature/refactor/etc. happens per-issue in Task 6 (strips type:enhancement there).
label_rename bug "type:bug" d73a4a
label_rename documentation "type:docs" 0075ca
label_rename refactoring "type:refactor" c5def5
label_rename research "type:research" fbca04
label_rename testing "type:test" bfd4f2
label_rename security "type:security" e11d48
label_rename auto-fix "super-qa:auto-fix" 27AE60
label_rename enhancement "type:enhancement" a2eeef
# 2. Upsert the full canonical set (idempotent; fixes colors incl. super-qa).
for f in labels.core.json labels.area.memory-engine.json; do
	jq -c '.[]' "$MD/$f" | while read -r l; do
		label_upsert "$(jq -r .name <<<"$l")" "$(jq -r .color <<<"$l")" "$(jq -r .description <<<"$l")"
	done
done
# design/performance/quality (folded per-issue) + 6 defaults are deleted in Task 9, gate-fenced.
