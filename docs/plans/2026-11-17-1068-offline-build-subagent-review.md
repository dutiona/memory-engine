Altitude report SHA-256: a87264a4b2022d00e1279264929d13e25dae3b340915723a56d333cf9cc04702 (I recomputed it from `.review/altitude.md` and it matches.)

# Review: build(container) offline build plan (#1068)

Reviewer: clean-slate Claude subagent (Opus), round 1.

**Verdict: LGTM with fixes. No blockers or HIGH findings.** Two MEDIUM findings should be fixed before item 2 and Operations step 3 run. The rest are LOW. Confidence is high: I read the plan, the altitude report, both inputs, and the snapshot's `Containerfile`, `.containerignore`, install guide, `AGENTS.md` Verify block, `Cargo.lock` and the crate manifests.

## Altitude and duplication (assessed first)

I agree with the report's **proceed**. The plan is about 151 lines for a change of about 16 lines, and I accept that override. The plan has no second copy of the product, no validator and no manifest. The fenced lines are the ticket's fixed text, quoted once in Constraints and referenced from item 1, not repeated. Most of the length is behavioural evidence, which a change to `Containerfile` alone can't get from a committed test. V1 is earned: V2 succeeding under `--network=none` can't tell whether buildah honoured the flag or silently ignored it, and V1 can.

## Findings

**[MEDIUM] Item 2: the new docs sentence credits `--offline` with something `--network=none` does.** The sentence says "`--offline` makes a crate the fetch missed, or a build script that needs the network, a build failure." `--offline` only controls cargo's own downloads and index resolution. A build script opening a socket fails because the step has no network, not because of `--offline`. Suggested wording: "`--network=none` makes a build script that needs the network fail the build; `--offline` makes cargo fail on a crate the fetch missed instead of trying to download it." The Outcome section has it right; only the docs prose is off.

**[MEDIUM] Operations step 3 and V1: quoting V1's interface lists in a public PR can leak host network details.** `/proc/net/dev` in the networked step shows the build's network namespace.
- Under rootless pasta, the tap device reuses the name of the host's default-route interface.
- If `containers.conf` sets the build network to `host`, it lists every host interface: bridges, VPN or tailnet devices, with byte counters.

"Scrubbed of paths and host names" doesn't cover this. Fix: say in the PR "networked step: `lo` + 1 other interface; offline step: `lo` only", or check the lists before posting and redact the names and counters.

**[LOW] Constraints and Operations step 3: `Refs dutiona/infra#37` points at a private repo.** I checked with `gh repo view`: `dutiona/infra` is PRIVATE and `dutiona/memory-engine` is PUBLIC. The line is owner-mandated and already in the public ticket, so outsiders learn nothing new and see only plain text. I'm accepting it. Note that the plan file carries it into public branch history too.

**[LOW] Constraints vs V2 and V3: `CARGO_BUILD_JOBS=16` doesn't reach the cargo inside `podman build`.** The cargo in the builder stage uses every CPU the build container sees, and with `lto = "thin"` and `codegen-units = 1` the release build is heavy. If the constraint is meant to bound load on the host, the plan needs something like `--cpuset-cpus`/`--cpus` on the two podman builds, without touching the `Containerfile`. If it only covers cargo run on the host (V5), say so. Michaël, you'll know which you meant.

**[LOW] Item 2: "Debian-signed packages from the base images' publisher" is imprecise for the builder.** `rust:1-slim-trixie` is a Docker Official Image, not a Debian-published one; its apt packages are Debian-signed through Debian's archive. "Debian-signed packages from the Debian archive" is accurate for both stages. The header comment in item 1 ("apt-get (Debian-signed packages)") is already correct. This touches only the docs prose, not the settled `Containerfile` lines.

**[LOW] V3: the `--ignored` precheck can fail for a harmless reason.** A `crates/<x>/target` left by a cargo run inside a crate makes it print something, but `.containerignore` excludes `**/target` anyway. Keep the check, since it catches ignored files that would otherwise enter the context, but note that `target` output is harmless rather than counting it as a failure.

## Checked and found sound

- **Removed risks hold:**
  - `Cargo.lock` has 0 git sources.
  - There is no `rust-toolchain*` file and no `.gitattributes`, so `git archive` drops nothing through `export-ignore`.
  - The only network-sensitive build-script crates in the lock are `openssl-sys` (pkg-config, covered by `libssl-dev`) and `ring`. Nothing like `ort` or `hf-hub` downloads at build time.
  - Both binaries are `version = "0.1.0"`, so the expected V2 and V4 lines match.
- **Item 1's static checks are right:**
  - The awk stops at the second `FROM`.
  - Comment lines are not counted.
  - The red output (one networked `RUN`) and green output (count `2`, two lines printed) are correct against the snapshot's `Containerfile`.
- **V2 is fine as written:**
  - `pipefail` inside the `flock` is correct.
  - `--no-cache` keeps stale 97c8e77 layers out.
  - The unanchored `STEP [0-9]+/[0-9]+: RUN` regex tolerates podman's `[n/m]` stage prefix.
  - The `git status` precheck correctly ties the evidence to committed content.
- **Sequencing works:** plan commit, then item 1, then item 2, then V1–V5, then PR. Item 2 is docs-only, so running the behavioural checks after it is fine. The rule for repeating checks after a review fix (a `RUN` or context change reruns V2–V4; a comment or docs change reruns the greps) is correct.
- **V5 is proportionate:** `AGENTS.md` says to run the whole cargo matrix after every change. Skipping the plugin gate and the MSRV job is justified and will be said in the PR.
- **Structure:** Documentation, Testing (no committed test, with the reason: a test parsing `Containerfile` would check text, not isolation) and Verification are all present. The acceptance map covers all four ticket criteria.
- **No over-engineering:** no abstractions, flags or extra files; `.containerignore` is unchanged, and the tar-context probe result shows that's enough.
- **Public-repo scan of the plan:** no home paths and no host names; the lock path and log directory stay as placeholders. The only private detail is the `dutiona/infra` reference above.

---

# Round 2 (delta), same reviewer

Altitude report SHA-256: be8c26b4300063f0af25f363164967de325c6373201d79cd93113fcfa6747c68

**Verdict: LGTM.** All six round-1 findings are resolved or explicitly accepted, the delta adds no new defects, and the refreshed altitude report is accurate. Both SHA-256 values match the files: plan.md is `02021479…e081`, and the report's own header names that hash.

- Altitude re-assessed: 157 lines, 11336 bytes, 2 tasks, 10 fenced lines / 1009 bytes all agree; the extra lines are verification wording and one constraint note, no second copy of the product. Disposition **proceed** accepted.
- [MEDIUM] docs sentence: resolved (loopback-only namespace and `--offline` now stated separately; item 2's green grep still holds).
- [MEDIUM] V1 leak: resolved (counts only; scrub list adds interface names).
- [LOW] infra reference: accepted unchanged as owner-mandated; the line is already in the public ticket.
- [LOW] `CARGO_BUILD_JOBS`: resolved by `--cpuset-cpus=0-15` (host-load bound only); I believe cargo's default jobs follow CPU affinity (not run).
- [LOW] Debian wording, [LOW] `--ignored` precheck: resolved.
- Codex's version change (no hard-coded `0.1.0`): correct.
- New findings: none.

## Resolution

- [MEDIUM] Item 2 docs sentence credited `--offline` with network isolation -> rewritten: the loopback-only namespace of `RUN --network=none` and `--offline` are stated as separate guarantees.
- [MEDIUM] V1 interface lists could leak host network details into the public PR -> V1 and Operations 3 report only interface counts, never names or counters.
- [LOW] `Refs dutiona/infra#37` names a private repository -> accepted unchanged: owner-mandated, already in the public ticket.
- [LOW] `CARGO_BUILD_JOBS=16` does not reach cargo inside `podman build` -> V2 and V3 add `--cpuset-cpus=0-15`, a host-load bound only, not part of the documented command.
- [LOW] "base images' publisher" imprecise for the builder -> "Debian-signed packages from the Debian archive".
- [LOW] V3 `--ignored` precheck could trip on `target/` -> `target/` entries named harmless (excluded by `**/target`); anything else is fixed first.
- Round 2: LGTM, no new findings.
