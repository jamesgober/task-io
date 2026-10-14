# task-io - Engineering Directives

> Engineering standards and the definition of done for this project. Read alongside `REPS.md` (root, authoritative), [`PLAN.md`](./PLAN.md) (design and decisions), [`BENCHMARKS.md`](./BENCHMARKS.md) (the performance contract), [`RESEARCH.md`](./RESEARCH.md) (the R&D program), and [`ROADMAP.md`](./ROADMAP.md) (current phase). If anything here conflicts with `REPS.md`, `REPS.md` wins, except where `PLAN.md` records an approved exception.

---

## 0. Philosophy

This library is built to a production standard and treated as a flagship. Jobs that send email, sweep sessions, and move records run through it, so a lost job here is a lost email, a stuck session, or a missing record. Plan the full path, measure before believing, then build one verified step at a time. "Good enough" is a defect. The bar is at least 1.20 times faster than the best of everything, and more dependable.

---

## 1. What this is

task-io is the task dispatch pipeline: workers, lanes, delays, cron, retries, dead letters, idempotency, leases, stores, and later dispatch across nodes. It is single-purpose. It is not a general async executor, an actor system, or a spawn framework, and none of those may be added to it (PLAN D-015). It does not own sockets, a storage engine, consensus, or a wire protocol. See `PLAN.md` section 3 for the full in/out list and where each excluded piece lives.

---

## 2. Engineering law (non-negotiable)

- **Ours, not rented.** No third-party runtime dependency. OS calls `std` lacks go through our own FFI declarations in `sys/`. Dev-only tooling (`criterion`, `proptest`, `loom` under `cfg(loom)`) is allowed. Competitor crates live only in `compare/`.
- **Unsafe is fenced.** `unsafe` only inside `queue/` (lock-free structures) and `sys/` (OS calls). Every block has a `// SAFETY:` comment stating the invariants, how the local code establishes them, and the platform contract (man page or Microsoft documentation) it relies on. Every `unsafe fn` has a `# Safety` section. `#![deny(unsafe_op_in_unsafe_fn)]` everywhere. Every module with `unsafe` has a `loom` model (for anything concurrent) and runs clean under Miri. Everything else is `#![deny(unsafe_code)]`. Until those modules exist the crate is `#![forbid(unsafe_code)]`.
- **Performance is a contract.** No change lands without the benchmarks it touches, run by the `BENCHMARKS.md` protocol. No "faster" claim without numbers. Allocation-free dispatch for small payloads; no lock on the submit, dispatch, or complete path that a lock-free design can avoid; no shared atomic written on every job when a per-worker one works.
- **Bounded everything.** Every lane, delayed set, dedupe index, store, DLQ, and retry loop has a limit, checked before memory is committed. No unbounded growth under any load.
- **The pool never dies from handler code.** Panics are caught at the handler boundary. A worker thread exits only at shutdown or when the elastic pool shrinks.
- **Every job is accounted for.** A job is pending, leased, completed, or dead-lettered. Nothing disappears.
- **Hostile-input hardening.** Payloads, cron expressions, durable records, and cluster messages are untrusted: size limits before allocation, fuzzed parsers and decoders, typed decode errors, no reachable panics, fail closed.
- **Cross-platform.** Linux, macOS, and Windows are first-class, verified by CI on stable and MSRV 1.85. Platform differences are documented per feature in `docs/PLATFORM-NOTES.md`.
- **Error handling.** Every fallible path returns `Result`; error enums are `#[non_exhaustive]`, implement `std::error::Error`, carry the cause as a source, and say what to do.
- **Production-ready.** `#![deny(missing_docs)]`; no `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, `dbg!`, or printing in library code; every public item has rustdoc with a runnable example; every claim in the docs maps to a test or a benchmark.
- **Simple API.** Tier 1 is four lines. Handlers are plain functions. One way to do each thing. A new public item needs a reason in `docs/API.md`.

---

## 3. Definition of done

A change, a phase, or a release is done only when every item holds:

1. Compiles clean, zero warnings, on Linux, macOS, and Windows, stable and MSRV 1.85.
2. `cargo fmt --all -- --check`; `cargo clippy --all-targets` with default, all, and no default features, `-D warnings`; `cargo test` with default, all, and no default features; `cargo doc --no-deps --all-features` with `RUSTDOCFLAGS=-D warnings`. All clean.
3. `cargo audit` and `cargo deny check` pass.
4. No `unwrap`/`expect`/`panic!`/`todo!`/`unimplemented!`/`dbg!` in shipping code; no commented-out code.
5. Every public item: doc comment, runnable example, tests for `Ok` and every `Err` path.
6. Every module with `unsafe`: `// SAFETY:` on every block, Miri clean (`cargo +nightly miri test`), `loom` model passing (`RUSTFLAGS="--cfg loom" cargo test --release`) for concurrent code.
7. Every invariant in section 4 that the change touches has a test that fails if the invariant breaks.
8. Every benchmark in `BENCHMARKS.md` that the change touches: run by the protocol on R1 and R2, gate passed, no regression over 5% against the baseline, results committed.
9. Property tests and deterministic-simulation seeds pass; any failing seed found is fixed and kept as a regression test.
10. Every store implementation touched passes the crash-at-every-step suite.
11. `README.md`, `docs/API.md`, and `CHANGELOG.md` updated; the matching `docs/release/vX.Y.Z.md` written before the tag.
12. `PLAN.md` decision log updated for every design decision; `RESEARCH.md` updated for every experiment.
13. v1.0.0 only after three consumers (the web stack's jobs, smtp-net, and one more) have run it end to end in production.

---

## 4. Project-specific invariants

Each one has at least one test, named after it, that would fail if it broke.

**Jobs**
- Every accepted job is in exactly one state at a time: pending, leased, completed, or dead-lettered.
- A completed or dead-lettered job never runs again (except by an explicit DLQ replay, which creates a new attempt record).
- A job's payload is never changed by the pipeline.
- A `JobId` is never reused within a store.

**Workers and lanes**
- No lost wakeups: a job made ready before a worker parks is run.
- Every lane gets its weighted share within the documented error; no ready job waits longer than the documented bound (aging).
- An idle pool makes zero periodic wakeups.
- A handler panic never ends a worker thread.
- Lane, delayed-set, dedupe, and DLQ sizes never exceed their limits.

**Time**
- A delayed or cron job never runs before its time.
- Lateness is bounded and documented per platform.
- A cancelled delayed job never runs.

**Failures and retries**
- Every failure is recorded with its reason and attempt number.
- A permanent failure never retries.
- Retries never exceed the configured count or the handler's retry budget.
- A job that exhausts its retries is in the DLQ with every attempt's reason.

**Idempotency and leases**
- Two runs of the same idempotency key inside its window never both commit.
- A result whose lease token is no longer current is never committed.

**Stores**
- With a durable store, a job acknowledged by `submit` survives a crash at any point.
- A corrupt record never runs; it goes to the DLQ with the reason.

**Shutdown**
- After `shutdown(deadline)` returns: no handler code runs, unstarted jobs are back in the store, and every worker thread is joined.

**Cluster (0.6.0)**
- A node runs only registered, versioned job types, and only from authenticated peers.
- No message is decoded before its size is checked against the limit.

---

## 5. Writing style

- Plain, specific, human prose in every shipping file. No marketing voice.
- Banned in shipping files: "comprehensive", "robust", "seamless", "leverage", and the em-dash character (U+2014).
- Numbers carry their unit and how they were measured. Estimates are marked as estimates until a benchmark replaces them.
- Commits: imperative present tense, descriptive subject line.
