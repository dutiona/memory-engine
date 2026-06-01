#!/usr/bin/env bash
source "$(dirname "$0")/lib.sh"
MD="$(dirname "$0")/manifests"
ST="$(dirname "$0")/state.json"
OID=$(owner_id)
create_or_get() {
	local e
	e=$(gql 'query($l:String!){user(login:$l){projectsV2(first:50){nodes{number title id}}}}' "$(jq -nc --arg l "$PM_OWNER" '{l:$l}')" --jq ".data.user.projectsV2.nodes[]|select(.title==\"$1\")|\"\(.number) \(.id)\"" | head -1)
	[ -n "$e" ] && {
		echo "$e"
		return
	}
	gql 'mutation($o:ID!,$t:String!){createProjectV2(input:{ownerId:$o,title:$t}){projectV2{number id}}}' "$(jq -nc --arg o "$OID" --arg t "$1" '{o:$o,t:$t}')" --jq '.data.createProjectV2.projectV2|"\(.number) \(.id)"'
}
field_id() { gql 'query($p:ID!){node(id:$p){... on ProjectV2{fields(first:50){nodes{... on ProjectV2SingleSelectField{id name}}}}}}' "$(jq -nc --arg p "$1" '{p:$p}')" --jq ".data.node.fields.nodes[]?|select(.name==\"$2\")|.id"; }
# Desired options as JSON, MERGING any existing option's id by name (fixes Codex C4: replacing a field's
# options WITHOUT echoing back existing ids recreates them and DROPS items' current Status assignments).
desired_opts() {
	local pid="$1" f="$2" ex
	ex=$(gql 'query($p:ID!){node(id:$p){... on ProjectV2{fields(first:50){nodes{... on ProjectV2SingleSelectField{name options{id name}}}}}}}' "$(jq -nc --arg p "$pid" '{p:$p}')" --jq "[.data.node.fields.nodes[]?|select(.name==\"$f\")|.options[]?]" 2>/dev/null)
	jq -c --argjson ex "${ex:-[]}" --arg f "$f" '[ .options[$f][] as $o | ((if ($ex|type)=="array" then $ex else [] end)|map(select(.name==$o[0]))[0]) as $m | {name:$o[0],color:$o[1],description:""} + (if $m then {id:$m.id} else {} end) ]' "$MD/projects.json"
}
set_status_opts() {
	local fid
	fid=$(field_id "$1" Status)
	gql 'mutation($f:ID!,$o:[ProjectV2SingleSelectFieldOptionInput!]!){updateProjectV2Field(input:{fieldId:$f,singleSelectOptions:$o}){projectV2Field{... on ProjectV2SingleSelectField{id}}}}' "$(jq -nc --arg f "$fid" --argjson o "$(desired_opts "$1" Status)" '{f:$f,o:$o}')" >/dev/null
}
create_select() {
	local fid
	fid=$(field_id "$1" "$2")
	[ -n "$fid" ] && return 0
	gql 'mutation($p:ID!,$n:String!,$o:[ProjectV2SingleSelectFieldOptionInput!]!){createProjectV2Field(input:{projectId:$p,dataType:SINGLE_SELECT,name:$n,singleSelectOptions:$o}){projectV2Field{... on ProjectV2SingleSelectField{id}}}}' "$(jq -nc --arg p "$1" --arg n "$2" --argjson o "$(desired_opts "$1" "$2")" '{p:$p,n:$n,o:$o}')" >/dev/null
}
read MN MI < <(create_or_get "Memory Engine — Main")
read TN TI < <(create_or_get "Memory Engine — Bug & Security Triage")
read RN RI < <(create_or_get "Memory Engine — Roadmap")
for PID in "$MI" "$RI"; do
	set_status_opts "$PID"
	create_select "$PID" Priority
	create_select "$PID" Phase
done
set_status_opts "$TI"
create_select "$TI" Priority
# projects.lock.json: numbers + node IDs (committed, reproducible from the create response).
jq -n --arg mn "$MN" --arg mi "$MI" --arg tn "$TN" --arg ti "$TI" --arg rn "$RN" --arg ri "$RI" \
	'{main:{number:$mn,id:$mi},triage:{number:$tn,id:$ti},roadmap:{number:$rn,id:$ri}}' >"$MD/projects.lock.json"
# state.json (fixes Codex C2 — concrete, ALL 3 projects): lock + per-project field/option-id maps. Gitignored,
# regenerable (re-run this script). Task 11 reads .<project>.fields.<Field>.{id,options.<value-name>}.
fmap() { gql 'query($p:ID!){node(id:$p){... on ProjectV2{fields(first:50){nodes{... on ProjectV2SingleSelectField{id name options{id name}}}}}}}' "$(jq -nc --arg p "$1" '{p:$p}')" --jq '[.data.node.fields.nodes[]?|select(.name)|{(.name):{id:.id,options:(reduce (.options[]?) as $o ({};.[$o.name]=$o.id))}}]|add'; }
jq -n --slurpfile lk "$MD/projects.lock.json" --argjson m "$(fmap "$MI")" --argjson t "$(fmap "$TI")" --argjson r "$(fmap "$RI")" \
	'$lk[0] * {main:{fields:$m},triage:{fields:$t},roadmap:{fields:$r}}' >"$ST"
