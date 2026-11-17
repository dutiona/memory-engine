# External review: Antigravity (gemini-3.8-flash-high), round 1

Michaël,

### Conclusion & Verdict
Plan `docs/plans/2026-11-17-1068-offline-build.md` is sound, minimal, and verified against the codebase. Confidence: High.

### Altitude & Duplication Assessment
- **Altitude Check**: The plan is 151 lines across 2 top-level implementation tasks for a ~16-line product diff (`Containerfile` and `docs/getting-started/installation.md`).
- **Justified Override**: The override is explicitly justified. The volume is concentrated in the behavioral verification protocol (V1–V5) necessary to prove container network isolation and offline build mechanics, rather than duplicating product code, schemas, or speculative abstractions.
- Altitude report SHA-256: a87264a4b2022d00e1279264929d13e25dae3b340915723a56d333cf9cc04702

### Detailed Analysis of Review Focus Areas
1. **Cargo fetch scope vs. offline build**: In the builder stage (`WORKDIR /src`, after `COPY . .`), `RUN cargo fetch --locked` resolves the virtual workspace manifest and fetches all crates from the committed `Cargo.lock` into `$CARGO_HOME` (`/usr/local/cargo`). Build scripts and proc-macros are not executed during fetch. The subsequent step uses `--offline` alongside `RUN --network=none`, ensuring any missing dependency or network attempt in `build.rs` fails immediately rather than hanging or attempting network I/O.
2. **CARGO_HOME persistence between RUN steps**: Successive `RUN` instructions within the `builder` stage commit layers containing `$CARGO_HOME`. The base image (`rust:1-slim-trixie`) defines no volume for `/usr/local/cargo`, so fetched crates persist into the build layer without requiring cache mounts.
3. **Tar-on-stdin build context**: `git archive --format=tar <ref> | podman build --pull=newer -t memory-engine:<tag> -` is tested in V2. Measured probe evidence (`.review/inputs/ctxprobe-RESULT.txt`) confirms `.containerignore`'s allowlist is honored in tar-on-stdin contexts. In V2, `bash -o pipefail` ensures pipeline failure propagation if `git archive` fails.
4. **Podman `RUN --network=none` semantics**: Buildah/Podman network unsharing is directly tested in V1 via an isolation probe inspecting `/proc/net/dev` (`lo` only under `--network=none`), preventing reliance on silent no-ops.
5. **Downstream line-anchored check**: Downstream consumers (`dutiona/infra#37`) requiring `^RUN --network=none ` match both offline steps (`Containerfile` builder step and runtime smoke step), with static grep coverage asserting a count of 2.
6. **Public repository hygiene**: No internal hostnames, private IPs, or user home paths exist in the plan or documentation changes.
7. **Structural completeness**: The plan contains complete `## Documentation`, `## Testing`, and `## Verification` sections.

[Antigravity Review] LGTM
REVIEW COMPLETE

---

# Round 2 (delta), resumed conversation

Degraded transport: the first round-2 invocation completed its review (LGTM) but the helper receipt was incomplete (final stream drain timed out after 1 s, cleanup unproven). After the worker verified no agy process remained, the same prompt was redispatched as round 2b, which completed with verified cleanup. Round 2b:

Michaël,

### Conclusion & Verdict
The Round 2b delta review of `docs/plans/2026-11-17-1068-offline-build.md` confirms that all refinements from the previous round remain sound, verified, and complete. Confidence: High.

### Altitude & Duplication Assessment
- **Altitude Check**: The plan remains at 157 physical lines across 2 top-level tasks with 4 code fences (10 lines, 1009 bytes).
- **Justification**: The altitude override is justified; it avoids duplicating product code while ensuring sufficient operational detail for container network isolation and verification evidence.
- Altitude report SHA-256: be8c26b4300063f0af25f363164967de325c6373201d79cd93113fcfa6747c68

### Delta Verification Summary
1. **Network vs. Offline distinction**: The documentation change in `docs/getting-started/installation.md` cleanly separates network namespace isolation (`RUN --network=none`) from cargo's offline enforcement mode (`--offline`). The apt repository provenance is accurately specified as the "Debian archive".
2. **CPU constraints (`--cpuset-cpus=0-15`)**: Verification builds properly restrict container cgroup CPUs to bound host load without leaking into the public documentation commands.
3. **Privacy protection**: Test evidence guidelines in V1 and Operations 3 restrict reported network details to interface counts rather than exposing host tap names.
4. **Output assertion decoupling**: Version smoke checks in V2 and V4 validate the expected format (`memory-engine-mcp <version>` and `memory-engine-cli <version>`) without hard-coding brittle version numbers.
5. **Pre-check tolerance**: V3 correctly accounts for `**/target` exclusions in `.containerignore` during clean checkout verification.

[Antigravity Review] LGTM
REVIEW COMPLETE


---

# Round 3, resumed conversation (restatement: the round-2b acknowledgement was a list item, the gate needs the exact line)

Michaël,

### Conclusion & Verdict
Plan `docs/plans/2026-11-17-1068-offline-build.md` remains unchanged (SHA-256 `02021479fb0bd85dced236e4e75334d0c8030586f1a9d59570b8f4aeacf0e081`), and all verifications from rounds 2 and 2b stand. Confidence: High.

### Altitude & Duplication Assessment
The plan stands at 157 lines across 2 top-level tasks. The altitude override is justified: it avoids duplicating product code while detailing the behavioral container network isolation tests (V1–V5) required for proof.

Altitude report SHA-256: be8c26b4300063f0af25f363164967de325c6373201d79cd93113fcfa6747c68

[Antigravity Review] LGTM
REVIEW COMPLETE

## Resolution

No changes required: LGTM in every round. Converged.
