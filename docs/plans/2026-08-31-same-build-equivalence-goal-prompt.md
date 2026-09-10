# Same-Build Client/Headless Equivalence Goal Prompt

Date: 2026-08-31

Status: Parked TODO — run before relying on headless execution for benchmarks,
state-aware fuzzing, multithreading work, or other correctness-sensitive tooling.
It does not block ongoing gameplay-parity work.

## Paste-Ready Goal

```text
/goal Establish bounded, non-tautological same-build client/headless simulation equivalence without freezing unfinished gameplay. The named reference is the production offline-skirmish simulation path at the base SHA: `app::match_runtime::sim_tick` through `SimRuntime::advance_frame`, including its match construction, resources, execution profile, and command admission. Headless must reproduce that path’s simulation outcomes for identical current-build inputs. This is an execution-harness goal, not a gamemd parity, stable-hash, replay-compatibility, fuzzing, benchmark, multithreading, rendering, or audio program.

Read and obey AGENTS.md and ENGINE.md. Reconcile Git, worktrees, branches, and running processes. Work in one isolated `feature/*` branch from current `origin/main`; preserve other tasks’ state.

First trace both real entry paths through construction and execution. Freeze a finite inventory covering match descriptor, map/rules identity, houses/starts, RNG initialization, cadence, command scheduling, tick lane, resources, and frame commit. Preserve matches and correct only confirmed plumbing divergence. The differential test must enter through both distinct production paths; never construct once and clone the runtime or call one helper twice.

Add one asset-free CI canary and one existing retail-backed stock-skirmish canary. Give both paths identical inputs, seed, commands, and an explicit production execution profile. Compare every committed step’s frame result, state hash, Scenario RNG cursor, LogicVector order, and final snapshot round-trip. Add a negative control proving that a deliberately different seed, cadence, or command schedule is detected. Compare both paths within the same test run; never pin an unfinished whole-game hash across commits.

Use shared concrete lower-layer construction where the trace proves it is the smallest fix. Preserve any native/cross-engine cadence tooling as an explicitly separate mode. Do not change verified gameplay merely to force equality, introduce mocks or one-implementation traits, redesign hashes/replays/snapshots, add GPU dependencies, or perform unrelated cleanup.

Scope fuse: if equivalence requires gameplay-semantic changes, snapshot/replay-schema migration, GPU ownership changes, or more than one independently reviewable construction boundary, stop implementation after producing a bounded design and exact transaction split. Do not expand the goal.

Run the smallest focused `--lib` tests. Give the requirement, base SHA, complete diff, and literal validation to a fresh read-only critic. Fix the largest confirmed gap and resubmit to a new critic until PASS, rechecking prior fixes. Run `$rust-scan --changed --base <base-sha>`; if ownership, dependencies, APIs, lifecycle seams, placement, or Cargo targets change, separately run `$architecture-scan --changed --base <base-sha>`. Confirmed change-caused CRITICAL or WARNING findings block completion.

Done only when both canaries independently exercise their real paths, same-build timelines match, negative controls prove sensitivity, reverse review finds no alternate execution path or frozen unfinished behavior, and one final `cargo test -p vera20k --lib` passes. Commit coherently; publish only as authorized.
```
