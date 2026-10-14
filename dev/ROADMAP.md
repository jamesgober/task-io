# task-io - Roadmap

> Path from scaffold to a stable 1.0. Hard parts are front-loaded; each phase has hard exit criteria, and a phase is not done until every box is ticked and every exit criterion is met.
> Design and decisions: [`PLAN.md`](./PLAN.md). Performance contract: [`BENCHMARKS.md`](./BENCHMARKS.md). R&D program: [`RESEARCH.md`](./RESEARCH.md). Standards and definition of done: [`DIRECTIVES.md`](./DIRECTIVES.md).
>
> **Anti-deferral rule:** no listed task moves to a later phase unless this file records the move and the reason, and James approved it.
>
> **Every phase also requires:** the full DIRECTIVES section 3 definition of done; every touched benchmark passing the BENCHMARKS gate on R1 and R2; CHANGELOG, README, `docs/API.md`, and `docs/release/vX.Y.Z.md` updated; decisions in PLAN section 14.

## v0.1.0 - Scaffold (DONE, 2026-10-13)
Compiles, CI green, structure correct, no domain logic.
- [x] Manifest (edition 2024, MSRV 1.85, no dependencies, `Apache-2.0 OR MIT`), README, CHANGELOG, canonical REPS, dual license, CI, deny, clippy, rustfmt, `.gitattributes`.
- [x] `dev/PLAN.md`, `dev/BENCHMARKS.md`, `dev/RESEARCH.md`, `dev/DIRECTIVES.md`, this roadmap.
- [x] Smoke test; `docs/API.md`; `docs/release/v0.1.0.md`.

## v0.2.0 - R&D and design lock
Exit: competitor baselines measured and committed, so every target in BENCHMARKS is a number; every RESEARCH entry I-01 to I-23 has a verdict with evidence; at least ten new entries added; at least three adopted capabilities no competitor has; James's spawn model written and specified; the public API for 0.3.0 to 0.5.0 written in `docs/API.md` and approved by James. No implementation code in `src/` beyond what the docs examples need to compile (the API may be stubbed only if every stub is unreachable from public code and returns a typed error).

**Step 1: the harness (first, before any prototype)**
- [ ] `compare/` crate: its own `[workspace]`, `publish = false`, competitor versions pinned, counting allocator, latency recorder, results writer.
- [ ] Suites E-01 to E-12, T-01 to T-02, C-01 to C-04, A-01 to A-10, J-01 to J-05, S-01 to S-04 implemented for every listed competitor, each competitor at its documented best (BENCHMARKS section 3).
- [ ] Baselines measured on R1 and R2; `compare/results/<date>-R1.md` and `-R2.md` committed; BENCHMARKS section 5 filled with the best competitor and the 1.20 target per row.

**Step 2: reading**
- [ ] Every source in RESEARCH section 5 read; one note per area in `.dev/research/`.

**Step 3: experiments**
- [ ] I-01 to I-23: prototype, measure, profile, verdict, evidence (RESEARCH section 3).
- [ ] At least ten new entries from reading, profiling, and failed experiments, each with a verdict.
- [ ] At least three adopted capabilities that no benchmarked competitor has, each with its winning measurement.
- [ ] Every adopted idea recorded in PLAN section 14.

**Step 4: the spawn model**
- [ ] James's spawn model written in PLAN 5.10 in his words (O-1).
- [ ] Specified: types, semantics, invariants (added to DIRECTIVES section 4), tests, benchmarks (added to BENCHMARKS with competitors or absolute targets).
- [ ] Placed in a phase (0.3.0 or 0.4.0) with its own checklist in this file.

**Step 5: design lock**
- [ ] `docs/API.md`: every public type, trait, function, and error variant for 0.3.0 to 0.5.0, with signatures, semantics, limits and their defaults with reasons, and an example per item.
- [ ] Every question in PLAN section 6 answered with its evidence.
- [ ] `unsafe` map: every `unsafe` operation planned in `raw/`, `queue/`, `sys/`, with its invariant and the loom or Miri test that covers it.
- [ ] `sys/` FFI surface per OS: every function, constant, and struct we declare, with its documentation reference.
- [ ] `docs/ARCHITECTURE.md` written from the final design.
- [ ] Open decisions O-1 to O-7 answered.
- [ ] James approves `docs/API.md`.

## v0.3.0 - Core
Exit: every E, T, C, and S benchmark passes the gate on R1 and R2; every task, scheduler, time, and shutdown invariant in DIRECTIVES section 4 has a passing test; loom and Miri clean; 10,000 simulation seeds pass per CI run.
- [ ] Error types (`Error`, `SpawnError`, `JoinError`, ...), `#[non_exhaustive]`, tested per variant.
- [ ] Task memory (`raw/`): size-class slabs, recycling, raw task pointers; Miri suite.
- [ ] Task header and state word; `JoinHandle`, `AbortHandle`; waker vtable; loom model of every transition.
- [ ] Queues (`queue/`): local ring deque, injector, eventcount; loom models; property tests against a model.
- [ ] Workers, LIFO slot, stealing order by topology, parking, searching-worker bound, poll budget.
- [ ] Topology and affinity in `sys/` for Linux, Windows, macOS.
- [ ] `Runtime`, `Builder`, `Handle`, runtime context, `block_on`, `spawn`, `try_spawn`, live-task limit.
- [ ] Blocking work with handoff and the spare-worker cap; watchdog and work rescue (if adopted).
- [ ] Fork-join: `join`, `scope` (blocking), and the async scope design from I-10.
- [ ] `spawn_local` and thread-local tasks.
- [ ] Cancellation token, abort, deadlines, `timeout`.
- [ ] Timer wheel; `sleep`, `interval`, `sleep_until`.
- [ ] `sync/`: oneshot, bounded MPSC, broadcast, notify, semaphore.
- [ ] Observability: per-worker counters, histograms, `EventSink`.
- [ ] `sim` feature: deterministic scheduler, clock, and randomness.
- [ ] Shutdown with deadline; drop from inside a task returns an error.
- [ ] CI adds: Miri job (nightly), loom job, ThreadSanitizer and AddressSanitizer jobs (Linux nightly), benches compile and run in test mode, simulation seeds job.
- [ ] Examples `01_quick_start.rs` to at least `05_*.rs`.
- [ ] `docs/PERFORMANCE.md` and `docs/PLATFORM-NOTES.md` started.

## v0.4.0 - Actors and supervision
Exit: A-01 to A-10 pass the gate on R1 and R2; every actor invariant has a passing test; supervision tested under panic storms.
- [ ] `Actor` trait, `Cx`, `Address` (location-transparent type), `tell`, `try_tell`, `ask` with deadline.
- [ ] Bounded mailbox; system-message lane.
- [ ] Lifecycle hooks: started, stopping, stopped, restarted.
- [ ] Supervision trees: one-for-one, one-for-all, rest-for-one; restart intensity; escalation; root supervisor.
- [ ] Links and monitors; registry by name.
- [ ] Handoff scheduling between actors (if adopted).
- [ ] Virtual actors (if adopted).
- [ ] The spawn model, if it was placed in this phase.

## v0.5.0 - Jobs
Exit: J-01 to J-05 pass the gate on R1 and R2; the crash-at-every-step suite passes for every store; 0 lost jobs and 0 duplicate effects across 10,000 fault-injected runs.
- [ ] Job registry, typed payload codec, size limits.
- [ ] Enqueue now, after, at; cron parser (fuzzed) and scheduler.
- [ ] Retry: exponential backoff with full jitter, cap, temporary/permanent classification hook.
- [ ] Dead-letter queue with reason and attempt history.
- [ ] Idempotency keys and dedupe window.
- [ ] Priority lanes with deficit round robin; per-tenant quotas.
- [ ] Leases with fencing tokens.
- [ ] `Store` trait; memory store; durable store per O-6; record checksums; optional encryption through `crypt-io`.
- [ ] Crash-test harness: kill at every step of the job state machine.

## v0.6.0 - Integration
Exit: a net-io echo server runs on task-io on all three OSes; the Hive DB executor prototype schedules morsels with locality hints on task-io; LexerSketch's `spawn` runs on task-io; service-kernel's supervised workers run on task-io; all gates still pass.
- [ ] `Driver` trait final; reference adapter in net-io (net-io's repository, depending on task-io).
- [ ] Locality hints on spawn (NUMA node, CCD, shard) for Hive DB REQ-035.
- [ ] LexerSketch scheduler binding; the stackful option if I-23 was adopted.
- [ ] service-kernel migration path, documented.

## v0.7.0 - Cluster
Exit: cluster benchmarks (BENCHMARKS 4.7, written before this phase starts) pass; the Jepsen-style suite passes (partitions, node loss, clock skew, message loss and duplication) with 0 lost jobs and 0 stale commits.
- [ ] Cluster workloads and competitors written into BENCHMARKS 4.7.
- [ ] Membership and leases through raft-io; transport through rpc-io over net-io with tls-net mutual authentication.
- [ ] Registered, versioned task types; remote spawn.
- [ ] Remote actors through `Address`.
- [ ] Distributed jobs with leases fenced by the raft term.
- [ ] Placement by load and locality.
- [ ] Decide feature vs adapter crate for the cluster layer, with compile-time and dependency measurements.

## v0.8.0 - Advance
Exit: second R&D round complete; every benchmark re-run; every gate passes; stretch (2.0) factor reported per benchmark.
- [ ] Re-read the field (new papers, new competitor releases); new RESEARCH entries.
- [ ] Re-measure competitors at their latest releases; targets updated upward if they improved.
- [ ] Adopt what wins; record everything.

## v0.9.0 - Hardening
Exit: no open finding.
- [ ] Fuzz targets for every decoder (job records, cron expressions, cluster messages), run for at least 24 hours each.
- [ ] 72-hour soak on R1 and R2, mixed workload, zero drift (S-04 extended).
- [ ] Chaos run: random panics, allocator failures, clock jumps, store crashes, network faults.
- [ ] Adversarial review; every finding fixed test-first; `.dev/AUDIT-0.9.0.md`.
- [ ] `docs/ARCHITECTURE.md`, `docs/PERFORMANCE.md`, `docs/PLATFORM-NOTES.md`, `docs/STABILITY-1.0.md` complete.
- [ ] `cargo-semver-checks` in CI.

## v1.0.0 - Stable
- [ ] Frozen after the web stack's jobs, the Hive DB executor, and LexerSketch or service-kernel have run it end to end in production.
- [ ] Every gate passes on the final build; results published in `docs/PERFORMANCE.md`.
