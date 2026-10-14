# task-io - Engineering Directives

> Engineering standards and the definition of done for this project. Read alongside `REPS.md` (root, authoritative), [`PLAN.md`](./PLAN.md) (design and decisions), [`BENCHMARKS.md`](./BENCHMARKS.md) (the performance contract), [`RESEARCH.md`](./RESEARCH.md) (the R&D program), and [`ROADMAP.md`](./ROADMAP.md) (current phase). If anything here conflicts with `REPS.md`, `REPS.md` wins, except where `PLAN.md` records an approved exception.

---

## 0. Philosophy

This library is built to a production standard and treated as a flagship. It is the place where every other crate's work runs, so a flaw here is a flaw everywhere, and a slow path here is a slow path everywhere. Plan the full path, measure before believing, then build one verified step at a time. "Good enough" is a defect. "Faster than tokio" is not the bar; "at least 1.20 times faster than the best of everything, and more stable" is.

---

## 1. What this is

task-io owns spawning, task memory, the scheduler, timers, cancellation, panic isolation, monitors, durable jobs, and distributed dispatch. Actors and spawns are separate crates built on it (`actor-io`, `spawn-io`). It does not own sockets, a reactor, file IO, a storage engine, consensus, or a wire protocol. See `PLAN.md` section 3 for the full in/out list and where each excluded piece lives.

---

## 2. Engineering law (non-negotiable)

- **Ours, not rented.** No third-party runtime dependency. OS calls `std` lacks go through our own FFI declarations in `sys/`. Dev-only tooling (`criterion`, `proptest`, `loom` under `cfg(loom)`) is allowed. Competitor crates live only in `compare/`.
- **Unsafe is fenced.** `unsafe` only inside `raw/` (task memory), `queue/` (lock-free structures), and `sys/` (OS calls). Every block has a `// SAFETY:` comment stating the invariants, how the local code establishes them, and the platform contract (man page or Microsoft documentation) it relies on. Every `unsafe fn` has a `# Safety` section. `#![deny(unsafe_op_in_unsafe_fn)]` everywhere. Every module with `unsafe` has a `loom` model (for anything concurrent) and runs clean under Miri. Everything else is `#![deny(unsafe_code)]`. Until those modules exist the crate is `#![forbid(unsafe_code)]`.
- **Performance is a contract.** No change lands without the benchmarks it touches, run by the `BENCHMARKS.md` protocol. No "faster" claim without numbers. Allocation-free hot paths; no lock on the spawn, wake, or poll path; no shared atomic written on every poll.
- **Bounded everything.** Every queue, channel, timer set, store, and retry loop has a limit, checked before memory is committed. No unbounded growth under any load.
- **The runtime never dies from user code.** Panics are caught at the task boundary. A worker thread exits only at shutdown.
- **Hostile-input hardening.** Job payloads and cluster messages are untrusted: size limits before allocation, fuzzed decoders, typed decode errors, no reachable panics, fail closed.
- **Cross-platform.** Linux, macOS, and Windows are first-class, verified by CI on stable and MSRV 1.85. Platform differences are documented per feature in `docs/PLATFORM-NOTES.md`.
- **Error handling.** Every fallible path returns `Result`; error enums are `#[non_exhaustive]`, implement `std::error::Error`, carry the cause as a source, and say what to do.
- **Production-ready.** `#![deny(missing_docs)]`; no `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, `dbg!`, or printing in library code; every public item has rustdoc with a runnable example; every claim in the docs maps to a test or a benchmark.
- **Simple API.** Tier 1 is three lines. One way to do each thing. A new public item needs a reason in `docs/API.md`.

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
10. `README.md`, `docs/API.md`, and `CHANGELOG.md` updated; the matching `docs/release/vX.Y.Z.md` written before the tag.
11. `PLAN.md` decision log updated for every design decision; `RESEARCH.md` updated for every experiment.
12. v1.0.0 only after three consumers (the web stack's jobs, the Hive DB executor, and LexerSketch or service-kernel) have run it end to end in production.

---

## 4. Project-specific invariants

Each one has at least one test, named after it, that would fail if it broke.

**Tasks**
- Every spawned task reaches exactly one end state: completed, cancelled, or panicked.
- A task's output is dropped exactly once; a task's future is dropped exactly once.
- A task is never polled by two threads at once and never polled after it completes.
- Task memory is freed only when no `JoinHandle`, `AbortHandle`, or waker refers to it; a waker never dangles.
- Waking a completed task is a no-op; waking from any thread, any number of times, is safe.

**Scheduler**
- No lost wakeups: a wake that happens before a worker parks results in that task being polled.
- Every runnable task is polled within a bounded number of scheduling rounds (no starvation); the bound is documented and tested.
- An idle runtime makes zero periodic wakeups.
- Blocking work never holds up async tasks beyond the bound in `docs/API.md` (S-02).
- Live task count never exceeds the configured limit.

**Time**
- A timer never fires before its deadline.
- Lateness is bounded and documented per platform.
- A cancelled timer never fires.

**Shutdown**
- After `shutdown(deadline)` returns: no user code runs, every task has reached an end state, every future and output has been dropped, and the thread count is back to the count before the runtime started.
- Dropping a runtime from inside one of its own tasks is detected and returns an error; it never deadlocks.

**Monitors and channels**
- A monitor receives exactly one exit per monitored task, with the cause; monitoring an ended task delivers its exit at once.
- Messages from one sender on one channel are received in send order.
- A full channel makes the sender wait or returns `Full`; it never grows past its limit.

**Jobs**
- A job acknowledged as durable is never lost, including across a crash at any point.
- Two runs of the same idempotency key inside its window never both commit an effect.
- A job whose lease expired cannot commit a result (fencing).
- A corrupt record never runs; it goes to the dead-letter queue with the reason.

**Cluster**
- A node runs only registered, versioned task types, and only from authenticated peers.
- No message is decoded before its size is checked against the limit.

---

## 5. Writing style

- Plain, specific, human prose in every shipping file. No marketing voice.
- Banned in shipping files: "comprehensive", "robust", "seamless", "leverage", and the em-dash character (U+2014).
- Numbers carry their unit and how they were measured. Estimates are marked as estimates until a benchmark replaces them.
- Commits: imperative present tense, descriptive subject line.
