#!/usr/bin/env bash
set -euo pipefail
PM_OWNER="${PM_OWNER:-dutiona}"
PM_REPO="${PM_REPO:-memory-engine}"
SLUG="$PM_OWNER/$PM_REPO"
THROTTLE="${GH_PM_THROTTLE:-0.25}"
gh_throttle() { sleep "$THROTTLE"; }
gh_ratecheck() {
	local r
	r=$(gh api rate_limit --jq '.resources.graphql.remaining' 2>/dev/null || echo 9999)
	[ "$r" -lt 100 ] && {
		echo "rate low ($r); sleep 60"
		sleep 60
	} || true
}

# CORRECT GraphQL invocation (fixes Codex C1 + C3). Two hazards this avoids:
#   C1: `gh api graphql` requires the document in a field named `query` — `-f q=...` would send a
#       VARIABLE named `q` and NO query. C3: typed array variables (e.g. singleSelectOptions) cannot
#       be passed via `-f`/`-F` (sent as a string → type-validation failure); they must travel inside a
#       JSON `variables` object. So: build {query,variables} with jq and pipe via `--input -`.
# Usage: gql '<query>' '<variables-json>' [extra gh args e.g. --jq '...']
gql() {
	local q="$1" vars="$2"
	shift 2
	gh_throttle
	jq -nc --arg q "$q" --argjson v "$vars" '{query:$q,variables:$v}' | gh api graphql --input - "$@"
}

label_upsert() {
	gh_ratecheck
	gh_throttle
	gh label create "$1" -R "$SLUG" -c "$2" -d "$3" --force
}
label_rename() {
	gh label list -R "$SLUG" --json name --jq '.[].name' | grep -qx "$1" || {
		echo "skip rename $1 (absent)"
		return 0
	}
	gh_throttle
	gh label edit "$1" -R "$SLUG" --name "$2" --color "$3"
}
owner_id() { gql 'query($l:String!){user(login:$l){id}}' "$(jq -nc --arg l "$PM_OWNER" '{l:$l}')" --jq '.data.user.id'; }
issue_node() { gh issue view "$1" -R "$SLUG" --json id --jq .id; }

# Idempotent item-add with PAGINATION (fixes Codex C5: Main will hold 105+ items, so first:100 misses
# existing items past page 1 → duplicate adds). Page until found, else add.
project_item_add() {
	local pid="$1" cid="$2" after=null page found
	while :; do
		page=$(gql 'query($p:ID!,$a:String){node(id:$p){... on ProjectV2{items(first:100,after:$a){pageInfo{hasNextPage endCursor} nodes{id content{... on Issue{id} ... on PullRequest{id}}}}}}}' "$(jq -nc --arg p "$pid" --argjson a "$after" '{p:$p,a:$a}')")
		found=$(echo "$page" | jq -r --arg c "$cid" '.data.node.items.nodes[]|select(.content.id==$c)|.id' | head -1)
		[ -n "$found" ] && {
			echo "$found"
			return 0
		}
		[ "$(echo "$page" | jq -r '.data.node.items.pageInfo.hasNextPage')" = true ] || break
		after=$(echo "$page" | jq '.data.node.items.pageInfo.endCursor') # quoted JSON string for next $a
	done
	gql 'mutation($p:ID!,$c:ID!){addProjectV2ItemById(input:{projectId:$p,contentId:$c}){item{id}}}' "$(jq -nc --arg p "$pid" --arg c "$cid" '{p:$p,c:$c}')" --jq '.data.addProjectV2ItemById.item.id'
}

# Epic link, QUERY-FIRST (fixes Codex-connector P1 / INV-IDEMPOTENT). Reads the child's current parent so a
# rerun is a true no-op and rollback records ONLY actual changes + the prior parent. Prints "skip" when already
# linked to this parent, else "changed <priorParentId-or-empty>"; the caller records {child, prior} only on "changed".
subissue_link() {
	local cur
	cur=$(gql 'query($c:ID!){node(id:$c){... on Issue{parent{id}}}}' "$(jq -nc --arg c "$2" '{c:$c}')" --jq '.data.node.parent.id // ""')
	[ "$cur" = "$1" ] && {
		echo skip
		return 0
	}
	gql 'mutation($p:ID!,$c:ID!){addSubIssue(input:{issueId:$p,subIssueId:$c,replaceParent:true}){issue{number}}}' "$(jq -nc --arg p "$1" --arg c "$2" '{p:$p,c:$c}')" >/dev/null
	echo "changed $cur"
}
