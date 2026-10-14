# task-io - Roadmap

> Path from scaffold to a stable 1.0. Hard parts are front-loaded; each phase has hard exit criteria, and a phase is not done until every box is ticked and every exit criterion is met.
> Design and decisions: [`PLAN.md`](./PLAN.md). Performance contract: [`BENCHMARKS.md`](./BENCHMARKS.md). R&D program: [`RESEARCH.md`](./RESEARCH.md). Standards and definition of done: [`DIRECTIVES.md`](./DIRECTIVES.md).
>
> **Anti-deferral rule:** no listed task moves to a later phase unless this file records the move and the reason, and James approved it.
>
> **Every phase also requires:** the full DIRECTIVES section 3 definition of done; every touched benchmark passing the BENCHMARKS gate on R1 and R2; CHANGELOG, README, `docs/API.md`, and `docs/release/vX.Y.Z.md` updated; decisions in PLAN section 14.
>
> **Order:** the web stack needs a local job queue and cron soon, so the local pipeline (0.3.0) comes first, durability second, distribution after both are proven.

## v0.1.0 - Scaffold (DONE, 2026-10-13)
Compiles, CI green, structure correct, no domain logic.
- [x] Manifest (edition 2024, MSRV 1.85, no dependencies, `Apache-2.0 OR MIT`), README, CHANGELOG, canonical REPS, dual license, CI, deny, clippy, rustfmt, `.gitattributes`.
- [x] `dev/PLAN.md`, `dev/BENCHMARKS.md`, `dev/RESEARCH.md`, `dev/DIRECTIVES.md`, this roadmap.
- [x] Smoke test; `docs/API.md`; `docs/release/v0.1.0.md`.

## v0.2.0 - R&D and design lock
Exit: competitor baselines measured and committed, so every target in BENCHMARKS is a number; every RESEARCH entry I-01 to I-22 has a verdict with evidence; at least ten new entries added; at least three adopted capabilities no competitor has; the public API for 0.3.0 and 0.4.0 written in `docs/API.md` and approved by James. No implementation code in `src/` beyond what the docs examples need to compile (the API may be stubbed only if every stub is unreachable from public code and returns a typed error).

**Step 1: the harness (first, before any prototype)**
- [ ] `compare/` crate: its own `[workspace]`, `publish = false`, competitor versions pinned, counting allocator, latency recorder, results writer.
- [ ] Survey: search crates.io for every job queue and worker pool crate; add any that could beat the listed competitors to the BENCHMARKS competitor set (never remove one).
- [ ] Suites W-01 to W-11, D-01 to D-02, R-01 to R-02, K-01, P-01 to P-03 implemented for every listed competitor, each at its documented best (BENCHMARKS section 3).
- [ ] Baselines measured on R1 and R2; `compare/results/<date>-R1.md` and `-R2.md` committed; BENCHMARKS section 5 filled with the best competitor and the 1.20 target per row.

**Step 2: reading**
- [ ] Every source in RESEARCH section 5 read; one note per area in `.dev/research/`.

**Step 3: experiments**
- [ ] I-01 to I-22: prototype, measure, profile, verdict, evidence (RESEARCH section 3).
- [ ] At least ten new entries from reading, profiling, and failed experiments, each with a verdict.
- [ ] At least three adopted capabilities that no benchmarked competitor has, each with its winning measurement.
- [ ] Every adopted idea recorded in PLAN section 14.

**Step 4: design lock**
- [ ] `docs/API.md`: every public type, trait, function, and error variant for 0.3.0 and 0.4.0, with signatures, semantics, limits and their defaults with reasons, and an example per item.
- [ ] Every question in PLAN section 6 answered with its evidence.
- [ ] The `Store` trait written so a durable store can group-commit and the crash suite can drive it.
- [ ] `unsafe` map: every `unsafe` operation planned in `queue/` and `sys/`, with its invariant and the loom or Miri test that covers it.
- [ ] `sys/` FFI surface per OS: every function, constant, and struct we declare, with its documentation reference.
- [ ] `docs/ARCHITECTURE.md` written from the final design.
- [ ] Open decisions O-1 to O-7 answered.
- [ ] James approves `docs/API.md`.

## v0.3.0 - Local pipeline
Exit: W, D, R, and K benchmarks pass the gate on R1 and R2; every job, worker, lane, time, failure, idempotency, lease, and shutdown invariant in DIRECTIVES section 4 has a passing test; loom and Miri clean; 10,000 simulation seeds pass per CI run; an example runs the web stack's first jobs (email with retries, cleanup, session-expiry sweep on cron).
- [ ] Error types (`Error`, `SubmitError`, `Fail`, ...), `#[non_exhaustive]`, tested per variant.
- [ ] Job, `JobId`, registry, payload with inline small payloads and the size limit.
- [ ] `queue/`: lane rings, eventcount; loom models; property tests against a model.
- [ ] Workers: placement by topology (`sys/` for Linux, Windows, macOS), batch dequeue, parking, elastic size (if adopted).
- [ ] Lanes with weights, deficit round robin, aging, tenant quotas.
- [ ] `Pool`, `Builder`, `register`, `submit`, `try_submit`, `submit_after`, `submit_at`, `cron`, the job builder.
- [ ] Back-pressure with a deadline.
- [ ] Timer wheel for delayed jobs; cron parser (fuzz target first) and next-fire.
- [ ] Failure classification, retries with backoff and full jitter, retry budgets, circuit breaker (if adopted).
- [ ] DLQ with attempt history; inspection and replay.
- [ ] Idempotency keys and the dedupe window.
- [ ] Leases, fencing tokens, watchdog, work rescue.
- [ ] `Store` trait and the memory store.
- [ ] `EventSink`, job events, per-worker counters, histograms.
- [ ] `sim` feature: deterministic clock, interleaving, store faults.
- [ ] Shutdown with deadline.
- [ ] CI adds: Miri job (nightly), loom job, ThreadSanitizer and AddressSanitizer jobs (Linux nightly), benches compile and run in test mode, simulation seeds job, cron fuzz smoke run.
- [ ] Examples `01_quick_start.rs` to at least `05_*.rs`.
- [ ] `docs/PERFORMANCE.md` and `docs/PLATFORM-NOTES.md` started.

## v0.4.0 - Durable
Exit: P-01 to P-03 pass on R1 and R2; the crash-at-every-step suite passes for the durable store; 0 lost jobs and 0 duplicate effects across 10,000 fault-injected runs; corrupt records always go to the DLQ.
- [ ] `Store` trait final; group commit (if adopted).
- [ ] Durable store per O-5; record format with checksums; recovery.
- [ ] Optional encryption at rest through `crypt-io`.
- [ ] Crash-test harness: kill at every step of the job state machine, for every store.
- [ ] Corruption suite.

## v0.5.0 - Integration
Exit: the web stack's jobs run on task-io with the durable store; smtp-net's retry and dead-letter path runs on task-io; service-kernel runs its recurring work on task-io; all gates still pass.
- [ ] Web stack integration example and guide (emails, cleanup, session-expiry sweeps).
- [ ] smtp-net failure classification mapped to `Fail::retry` and `Fail::permanent`.
- [ ] service-kernel integration, documented.
- [ ] `Executor` seam for async handlers, at zero cost when unused.

## v0.6.0 - Cluster
Exit: cluster benchmarks (BENCHMARKS 4.7, written before this phase starts) pass; the Jepsen-style suite passes (partitions, node loss, clock skew, message loss and duplication) with 0 lost jobs and 0 stale commits.
- [ ] Cluster workloads and competitors written into BENCHMARKS 4.7.
- [ ] Membership and leases through raft-io; transport through rpc-io over net-io with tls-net mutual authentication.
- [ ] Registered, versioned job types; remote dispatch.
- [ ] Leases fenced by the raft term.
- [ ] Placement by load and locality.
- [ ] Decide feature vs adapter crate for the cluster layer, with compile-time and dependency measurements.

## v0.7.0 - Advance
Exit: second R&D round complete; every benchmark re-run; every gate passes; stretch (2.0) factor reported per benchmark.
- [ ] Re-read the field (new papers, new competitor releases); new RESEARCH entries.
- [ ] Re-measure competitors at their latest releases; targets updated upward if they improved.
- [ ] Adopt what wins; record everything.

## v0.9.0 - Hardening
Exit: no open finding.
- [ ] Fuzz targets for every parser and decoder (cron, payload codec, durable records, cluster messages), run for at least 24 hours each.
- [ ] 72-hour soak on R1 and R2, mixed workload, zero drift (S-01 extended).
- [ ] Chaos run: random panics, stuck handlers, allocator failures, clock jumps, store crashes, network faults.
- [ ] Adversarial review; every finding fixed test-first; `.dev/AUDIT-0.9.0.md`.
- [ ] `docs/ARCHITECTURE.md`, `docs/PERFORMANCE.md`, `docs/PLATFORM-NOTES.md`, `docs/STABILITY-1.0.md` complete.
- [ ] `cargo-semver-checks` in CI.

## v1.0.0 - Stable
- [ ] Frozen after the web stack's jobs, smtp-net, and one more consumer have run it end to end in production.
- [ ] Every gate passes on the final build; results published in `docs/PERFORMANCE.md`.
