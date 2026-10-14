# task-io - Project Plan

> The design, strategy, and decision record for `task-io`. This is a living document: any design decision, idea, or open question about task-io that comes up anywhere (chat, review, a sibling crate) is written here, in [`RESEARCH.md`](./RESEARCH.md), or in [`ROADMAP.md`](./ROADMAP.md) before it is acted on, so nothing lives only in someone's memory.
>
> **Read order:** [`../REPS.md`](../REPS.md) (portfolio standard) -> [`DIRECTIVES.md`](./DIRECTIVES.md) (engineering law for this crate) -> **this file** -> [`BENCHMARKS.md`](./BENCHMARKS.md) (the performance contract) -> [`RESEARCH.md`](./RESEARCH.md) (the R&D program) -> [`ROADMAP.md`](./ROADMAP.md) (phases and exit criteria).
>
> **Status:** scaffold, `0.1.0`. R&D and design lock is `0.2.0`. Everything in sections 5 and 6 is the **planned** shape; `0.2.0` either confirms each part with measurements or replaces it, and records why in section 14.

---

## Contents

1. [Purpose](#1-purpose)
2. [Where task-io sits](#2-where-task-io-sits)
3. [Scope](#3-scope)
4. [Mandates](#4-mandates)
5. [Architecture (planned)](#5-architecture-planned)
6. [API shape (planned)](#6-api-shape-planned)
7. [Failure model](#7-failure-model)
8. [Security responsibilities](#8-security-responsibilities)
9. [Ecosystem and dependencies](#9-ecosystem-and-dependencies)
10. [Phases](#10-phases)
11. [Testing and verification](#11-testing-and-verification)
12. [Open decisions](#12-open-decisions)
13. [Ideas](#13-ideas)
14. [Decision log](#14-decision-log)

---

## 1. Purpose

task-io is the place where work runs. Anything in the portfolio that wants something done concurrently hands it to task-io: an async task, a blocking call, a CPU-heavy fork-join computation, a job that has to survive a crash, or, later, work that should run on another node. Actors (`actor-io`) and spawns (`spawn-io`) are separate crates built on top of it (D-015).

It owns:

- spawning, and the memory every task lives in,
- the scheduler: worker threads, run queues, work stealing, parking and waking, CPU topology,
- timers, deadlines, and cancellation,
- panic isolation, and monitors that tell a supervisor when and why a task ended,
- durable jobs: delayed, recurring, retried, dead-lettered, idempotent,
- distributed dispatch: placing tasks and jobs on other nodes (later phase).

The goal is not to be another runtime. The goal is to be the fastest, most stable, and simplest task runtime available in any language, measured, with the numbers published. Section 4 turns that into hard rules.

---

## 2. Where task-io sits

### 2.1 Who uses it

Every consumer below already exists or is planned, and each one has written down what it needs. These are the real use cases the design is checked against.

| Consumer | What it needs from task-io | Where it is written |
|---|---|---|
| Web stack (`serve-net`, `web-net`, `gate-net`) | background jobs: emails, cleanup, session-expiry sweeps; a local job queue and cron first, distribution later | `_strategy/WEB-STACK-DELIVERABLES.md` section 7 |
| `smtp-net` | queuing, retries with backoff, and dead letters for outgoing mail; smtp-net classifies failures as temporary or permanent and task-io acts on it | `net-collection/smtp-net/dev/PLAN.md` |
| Hive DB executor (`hive-exec`) | a core scheduler that drives parallel query execution; scans split into morsels with locality hints (NUMA node, shard); engines spawn no threads of their own; work stealing and fair scheduling | `Hive DB/planning/engine-boundary/01d-interface-requirements.md` REQ-035; `hive-exec/.dev/PLANNING.md` 0.8.0 |
| LexerSketch runtime | a scheduler component for languages whose concurrency model is threads, tasks (async), or actors; `spawn` in a script binds to it; no rented dependencies | `lang-collection/_lexersketch/ARCHITECTURE.md` section 7; `NEW-LIBS.md` |
| `service-kernel` | supervised workers, watchdog, circuit breaker; today these sit on tokio | `service-kernel/src/worker` |
| `net-io` | a place to run connection handlers; its reactor (0.5.0) wakes tasks through task-io's driver seam | `net-io/dev/PLAN.md` |
| `store-io`, `raft-io`, `rpc-io` | completion handling and background work without each crate running its own threads | their plans |
| `spawn-io` (planned) | the base for James's spawns: monitored tasks, exit causes, deadlines, placement, blocking work, the job store as a mission ledger | `_strategy/SPAWN-IO.md` section 5 |
| `actor-io` (planned, if needed) | the base for message-only actors | `_strategy/SPAWN-IO.md` S-2 |

### 2.2 Layers

```
      application code, services, Hive DB, LexerSketch programs, spawn-io, actor-io
                                   |
   +----------------------------------------------------------------+
   |  L3  cluster: remote spawn, distributed jobs                    |  0.6.0
   |  L2  jobs: durable queue, delay, cron, retry, DLQ, idempotency  |  0.4.0
   |  L1  core: tasks, scheduler, timers, cancellation, monitors     |  0.3.0
   +----------------------------------------------------------------+
        |  driver seam (trait)            |  storage seam (trait)   |  transport seam (trait)
     net-io reactor                  memory / store-io          rpc-io + tls-net + raft-io
```

Each layer only uses the public API of the layer below it. A user who only wants L1 pays nothing for L2 and L3: they are behind additive features and add no code, no memory, and no branches to L1's hot paths.

---

## 3. Scope

### 3.1 In scope

- Spawning async tasks (`Future`), blocking closures, and CPU fork-join work on one scheduler.
- Thread-local (`!Send`) tasks pinned to one worker.
- Structured concurrency (scopes), cancellation, deadlines, join and select helpers.
- A timer wheel: sleep, interval, deadline, timeout.
- The synchronization primitives the runtime itself needs, made public: oneshot, bounded MPSC, broadcast, notify, semaphore, and a cancellation token.
- Task monitors: exit notification with the cause, the base that supervisors in `spawn-io` and `actor-io` are built on.
- Durable jobs with a pluggable store.
- Distribution across nodes with a pluggable transport and membership.
- A deterministic simulation mode for testing (section 5.15).
- Observability hooks: counters, histograms, and task events, with no cost when no sink is attached.

### 3.2 Out of scope, and where it goes

| Not here | Where it lives | Why |
|---|---|---|
| Sockets, IO readiness, the reactor | `net-io` | net-io owns sockets; task-io exposes a driver seam it plugs into (D-004) |
| File IO | `fsys` / `store-io` | blocking file IO runs as blocking work on task-io; the IO itself is theirs |
| Storage engine for durable jobs | `store-io` (or memory) | task-io defines the store trait; it does not write a storage engine |
| Consensus and membership | `raft-io` | task-io's cluster layer uses it through a trait |
| Wire protocol and encryption between nodes | `rpc-io`, `tls-net` | same |
| Logging and metrics backends | `log-io`, `metrics-lib` | task-io emits through a sink trait |
| Application protocols, HTTP | the `-net` crates | not a runtime concern |
| Spawns (spawner, missions, ledger, replacement, elastic replication) | `spawn-io` (planned) | James's design, built on task-io; see `_strategy/SPAWN-IO.md` |
| Actors, mailboxes, supervision trees | `actor-io` (planned, if needed) | single responsibility: task-io runs work, it does not define programming models on top of it |

---

## 4. Mandates

These are requirements, not goals. A release that misses any of them does not ship. They came from James on 2026-10-13 (D-006, D-011, D-012, D-013) and only James can change them, in writing, in section 14.

### 4.1 Performance

- **Faster than everything, by a margin.** On every benchmark in [`BENCHMARKS.md`](./BENCHMARKS.md), task-io's primary metric must beat the best competitor by a speedup factor of **at least 1.20** (20%), measured by the protocol in that file. The stretch target is 2.0. Losing to, or tying with, any competitor on any gated benchmark is a release blocker.
- **Lowest overhead.** Steady-state spawn of a small task makes zero heap allocations. An idle runtime makes zero wakeups. Per-task memory beats every competitor by the same 1.20 factor.
- **Tail latency is a first-class metric.** p99 and p99.9 are gated, not only p50.
- **No regressions.** After a benchmark passes, a change that makes it more than 5% slower is rejected (REPS performance gate).

### 4.2 Stability and safety

- **The runtime never dies because of user code.** A panic in a task or job is caught at the task boundary and returned as an error or reported to its monitor; the worker thread keeps running.
- **Nothing is unbounded.** Every queue, channel, timer set, job store, and retry loop has a limit that is checked before memory is committed. Overload produces back-pressure or a typed rejection, never unbounded growth.
- **No lost work.** A task reaches exactly one end state. A job that was acknowledged as durable is never lost, including across a crash at any instruction.
- **Fail closed and fail visible.** Every error is typed, `#[non_exhaustive]`, and says what to do. No silent drops.
- **Proven, not hoped.** Every lock-free structure has a `loom` model; every `unsafe` block runs under Miri; every invariant in DIRECTIVES section 4 has a test that would fail if it broke.

### 4.3 Simplicity

- **The first screen of the docs is three lines of code.** `Runtime::new()`, `spawn`, `block_on`. Correct defaults for every limit; a user who never opens the builder gets a safe, fast runtime.
- **One way to do each thing.** No two public functions with overlapping jobs.
- **Errors you can act on.** Each error variant names the limit or state that caused it.
- **Three tiers.** Tier 1 (the defaults, a handful of functions) headlines the docs; Tier 2 (builders and knobs) is one click away; Tier 3 (seams for drivers, stores, and transports) is for crate authors.

### 4.4 Testing

- Every public item: unit tests for the `Ok` and every `Err` path, a runnable doc example.
- Every concurrent structure: `loom` model plus stress tests on all three operating systems.
- Every invariant: a property test or a deterministic-simulation test.
- Every failure mode in section 7: a test that injects it.
- Every claim in a doc: a test or benchmark that proves it. Full list in section 11.

---

## 5. Architecture (planned)

This is the starting design. R&D in `0.2.0` ([`RESEARCH.md`](./RESEARCH.md)) measures it against alternatives; whatever wins is what gets built.

### 5.1 Module map (planned)

```
src/
  lib.rs          crate root, lints, Tier 1 re-exports
  error.rs        Error, SpawnError, JoinError, ... (#[non_exhaustive])
  runtime/        Runtime, Builder, Handle, context, shutdown
  task/           task header, state word, JoinHandle, AbortHandle, waker vtable
  raw/            (unsafe) task memory: size-class slabs, recycling, raw task pointers
  sched/          workers, run queues, LIFO slot, stealing, parking, topology, budgets
  queue/          (unsafe) lock-free queues: local ring deque, injector, MPSC, eventcount
  time/           timer wheel, sleep, interval, timeout
  sync/           oneshot, mpsc, broadcast, notify, semaphore, cancel token
  scope/          structured concurrency
  blocking/       blocking work, compensation workers
  local/          !Send tasks
  sim/            deterministic simulation scheduler (feature `sim`)
  monitor/        task monitors and exit causes (the base for spawn-io and actor-io)
  job/            (feature `job`) Job, queue, schedule, retry, DLQ, Store trait, memory store
  cluster/        (feature or adapter crate, decided at 0.6.0) remote spawn, distributed jobs
  observe/        counters, histograms, event sink trait
  sys/            (unsafe) OS calls std lacks: topology, affinity, futex / WaitOnAddress
```

`unsafe` is allowed only in `raw/`, `queue/`, and `sys/` (DIRECTIVES section 2).

### 5.2 The task

- **One allocation per task, then none.** A task is a header plus the future plus the output slot, carved from a per-worker size-class slab. Freed tasks go back to the slab of the worker that freed them; slabs refill across workers in batches. Target: zero heap allocations per spawn in steady state for futures up to the largest inline size class (BENCHMARKS E-10).
- **One state word.** Scheduling state, completion, cancellation, join interest, and the reference count live in one atomic word, so the common transitions (notified -> running -> idle, running -> complete) are one compare-and-swap each.
- **Wakers without refcount churn.** `wake_by_ref` from the running worker does not touch the reference count; the vtable is static per task type.
- **Exactly one end state.** Completed (output stored), cancelled, or panicked (payload stored). The output is dropped exactly once, on the thread that drops it.

### 5.3 The scheduler

- **One worker per core by default**, placed by topology (5.4). Each worker has a fixed-size local ring deque, a single-task LIFO slot for message-passing locality, and a timer wheel.
- **Global injector** for spawns from outside the runtime, drained in batches.
- **Work stealing:** steal half of a victim's queue; victims chosen nearest-first by topology (same core complex before another, same NUMA node before another).
- **Parking:** a bounded number of searching workers; idle workers park on an eventcount so no wakeup is lost and no herd is woken. Spin briefly before parking only when measurement shows it pays (E-08 in RESEARCH).
- **Fairness:** each poll runs on a budget; the LIFO slot is bounded so ping-pong pairs cannot starve the queue; the injector and timers are checked at a fixed interval.
- **Tickless idle:** no periodic timer when nothing is due. An idle runtime uses zero CPU.

### 5.4 CPU topology and placement

Topology is read at start through `sys/` (Linux sysfs, Windows `GetLogicalProcessorInformationEx`, macOS `sysctl`): cores, SMT siblings, L3 groups (AMD CCDs, Intel tiles), and NUMA nodes. It drives worker placement, steal order, and slab refill order. On the reference machine (Ryzen 9 9950X3D: two CCDs, one with 3D V-Cache), crossing CCDs costs far more than staying inside one, so related tasks are kept on one CCD unless load requires otherwise. Pinning is optional (Tier 2) because it hurts on shared hosts.

### 5.5 Kinds of work, one pool

| Kind | API (planned) | How it runs |
|---|---|---|
| async task | `spawn(fut)` | polled by workers |
| blocking call | `spawn_blocking(f)` | runs on a worker that is handed off: the worker's queue moves to a spare worker so async tasks keep running (Go-style handoff), and the spare count is capped |
| CPU fork-join | `join(a, b)`, `scope(|s| ...)` | rayon-style: the second half is pushed to the local deque and run inline if nobody stole it; evaluated against heartbeat scheduling in RESEARCH I-04 |
| thread-local | `spawn_local(fut)` | stays on one worker; no atomics on its hot path |
| job | `jobs().enqueue(..)` (L2) | stored, then run as a task when due |

One pool means one set of threads, one memory budget, one place to measure. If R&D shows a split pool wins a gated benchmark, the split is adopted and recorded.

### 5.6 Structured concurrency, cancellation, deadlines

- `scope` joins every child before it returns, so children may borrow from the parent. Blocking scopes are sound today; async scopes run into the scoped-task trilemma (a future can be leaked with `mem::forget`). RESEARCH I-10 decides the async design; nothing unsound ships.
- Cancellation is cooperative and explicit: a `CancelToken` is passed down; aborting a task drops its future at the next await point; cancellation is never silent (the join returns `Cancelled`).
- Deadlines attach to a task or scope; every blocking and waiting API in the crate takes one or inherits it.

### 5.7 Timers

Hierarchical timing wheel per worker (Varghese and Lauck), O(1) insert and cancel, coarse levels for far timers, lazy cancellation, and no global lock. Timers never fire early. Lateness and resolution are measured (BENCHMARKS T-01, T-02).

### 5.8 Channels and synchronization

The runtime needs oneshot (join results), bounded MPSC (work hand-off between tasks), notify (parking), and a semaphore (limits). They are written once, tuned for the scheduler (a send that wakes a receiver on the same worker uses the LIFO slot), and made public so users do not need another crate.

### 5.9 Monitors

A monitor is how code above task-io learns that a task ended, and why, without polling. It is the one primitive every supervisor needs, so task-io provides it once and does not build supervisors itself.

- `monitor(&handle)` returns a receiver that gets exactly one `Exit` per monitored task: `Completed`, `Cancelled`, or `Panicked` (with the payload), plus the task id and the time it ended.
- Any number of monitors per task; monitoring a task that already ended delivers its exit immediately.
- The exit is delivered by the worker that finished the task, with no extra thread and no allocation on the task's hot path (RESEARCH I-13).
- Restart policies, mission ledgers, mailboxes, and supervision trees are not here; they live in `spawn-io` and `actor-io` (D-015).

### 5.10 What spawn-io and actor-io need from task-io

James's spawn concept (spawner, missions, a mission ledger, instant replacement that resumes at the last committed step, elastic replication) is written down in `_strategy/SPAWN-IO.md`. It belongs to `spawn-io`, a separate crate built on task-io. Actors, if they are built, belong to `actor-io`. task-io's job is to give both of them what they need, so neither has to change task-io later:

| Need | task-io provides | Where |
|---|---|---|
| know when and why a spawn ended | monitors with the exit cause and panic payload | 5.9 |
| replace a stuck spawn | abort, deadlines, timers for heartbeats and step budgets | 5.6, 5.7 |
| spawns that use files and services | blocking work that never stalls async tasks | 5.5 |
| keep a spawner's spawns close together | placement hints (core complex, NUMA node) | 5.4 |
| talk between spawner and spawns | bounded channels | 5.8 |
| a durable mission ledger | the job `Store` trait | 5.11 |
| test replay at every step | the deterministic simulation mode | 5.15 |

Any change to this list is agreed with James, because it shapes his design.

### 5.11 Jobs (L2)

- `Job` handlers are registered by name with a typed payload; payloads are bytes to the store, decoded by the handler.
- Enqueue now, after a delay, at a time, or on a cron schedule.
- Retries with exponential backoff and full jitter, a cap, and a classification hook (temporary vs permanent, which smtp-net needs).
- Dead-letter queue with the reason and attempt history.
- Idempotency keys with a dedupe window: running the same key twice inside the window is refused.
- Priority lanes scheduled by deficit round robin, so low priority work is slower but never starved.
- Leases (visibility timeouts) with fencing tokens, so a job whose worker stalled is retried and the stale worker's late result is rejected.
- A `Store` trait. A memory store ships in the crate. A durable store uses a first-party storage crate (O-6). Every store implementation must pass the same crash-test suite (kill at every step).
- Delivery guarantee, stated precisely: **at-least-once execution, at-most-once effect per idempotency key within the window.** Exactly-once is not claimed because it cannot be guaranteed for effects outside the store.

### 5.12 Distribution (L3)

- Membership and leases through `raft-io`; transport through `rpc-io` over `net-io` with `tls-net` (mutual authentication); all behind traits.
- Remote spawn runs only **registered** task types, named and versioned; no code or closures cross the wire.
- Distributed jobs with leases fenced by the raft term.
- Placement by load and locality; work moves to idle nodes in batches.
- Partitions, node loss, and clock skew are tested with a Jepsen-style harness before the layer ships (0.6.0).

### 5.13 Driver seam (IO)

task-io does not do IO. It exposes a `Driver` trait: on park, a worker asks the driver for ready events with a timeout instead of sleeping, so a reactor (net-io's epoll, kqueue, IOCP, or io_uring) wakes tasks with no extra thread and no extra hop. Without a driver, workers park on the eventcount.

### 5.14 Observability

Per-worker counters (no shared atomics on the hot path), sampled histograms (poll time, queue wait, steal count), and task lifecycle events delivered to an `EventSink` trait. With no sink attached, the cost is one predictable branch; BENCHMARKS measures the cost with a sink attached.

### 5.15 Deterministic simulation

A `sim` feature replaces the scheduler, clock, and randomness with a single-threaded, seeded version. The same seed replays the same interleaving, so a race found once can be replayed forever. task-io uses it to test itself, and users get it to test their own code (FoundationDB and TigerBeetle use this approach; none of the runtimes in BENCHMARKS offers it built in).

### 5.16 Limits and defaults (proposed, finalized at 0.2.0)

| Limit | Default | Guards against |
|---|---|---|
| workers | one per physical core | oversubscription |
| live tasks | 1,048,576 | memory exhaustion from a spawn loop |
| local queue | 256 per worker (power of two) | memory per worker; steal granularity |
| injector batch | 32 | external spawn contention |
| poll budget | 128 operations per poll | one task starving others |
| spare (handoff) workers | equal to workers | thread explosion from blocking work |
| channel | 1,024 messages | a slow receiver holding unbounded memory |
| timers | 1,048,576 per runtime | timer floods |
| job retries | 10, backoff capped at 1 hour | retry storms |
| dedupe window | 24 hours | duplicate effects |

Every default is measured in `0.2.0`, and each one is written down in `docs/API.md` along with the reason for its value.

---

## 6. API shape (planned)

This is the planned shape. `docs/API.md` locks it at `0.2.0`.

**Tier 1: the defaults.**

```rust
use task_io::Runtime;

fn main() -> Result<(), task_io::Error> {
    let rt = Runtime::new()?;                       // one worker per core, safe limits
    let answer = rt.block_on(async {
        let a = task_io::spawn(async { 20 });
        let b = task_io::spawn_blocking(|| 22);     // never stalls async tasks
        Ok::<_, task_io::Error>(a.await? + b.await?)
    })?;
    assert_eq!(answer, 42);
    Ok(())
}
```

**Monitors.**

```rust
let handle = task_io::spawn(work());
let exit = task_io::monitor(&handle);              // exactly one Exit per task
match exit.recv().await? {
    Exit::Completed(_) => {}
    Exit::Panicked { id, payload, .. } => restart(id, payload),
    Exit::Cancelled(_) => {}
}
```

**Jobs (L2).**

```rust
rt.jobs().register("send_email", send_email)?;
rt.jobs()
    .enqueue("send_email", payload)
    .after(Duration::from_secs(30))
    .key(idempotency_key)
    .submit()?;
rt.jobs().cron("0 3 * * *", "cleanup_sessions")?;
```

**Questions 0.2.0 must answer with evidence, with the current recommendation:**

| Question | Recommendation | Deciding evidence |
|---|---|---|
| What does `spawn` do when the live-task limit is reached? | `spawn` waits for capacity in async context; `try_spawn` returns `SpawnError::Full`; outside async context `spawn` returns `SpawnError::Full` | no unbounded growth (4.2), simplest correct behavior |
| What does `task_io::spawn` do outside a runtime? | returns a handle that resolves to `JoinError::NoRuntime`; never panics | no reachable panics |
| Does dropping a `JoinHandle` detach or cancel? | detach (like `std::thread`), with `scope` as the structured alternative | least surprise for users coming from std and tokio |
| Does a monitor see the output value, or only the cause? | only the cause and the task id; the output goes to the `JoinHandle` | one owner per value, no clone on exit |
| Free functions (`task_io::spawn`) vs handle methods only | both: free functions use the current runtime context, `Handle` methods for code outside it | Tier 1 brevity |

---

## 7. Failure model

What can go wrong, and what task-io guarantees when it does.

| Failure | Guarantee | Test |
|---|---|---|
| a task panics | the panic is caught at the task boundary; `JoinHandle` returns `JoinError::Panicked` with the payload; the worker keeps running | unit, S-03 panic storm |
| a monitored task panics | every monitor receives `Exit::Panicked` with the payload, exactly once; what happens next is the supervisor's decision (spawn-io, actor-io) | monitor suite, E-13 |
| a task blocks a worker without `spawn_blocking` | a watchdog detects a poll longer than the threshold, reports it, and moves that worker's queue to a spare worker | S-02 |
| spawn loop / message flood / timer flood | limits refuse or delay new work before memory is committed | overload suite S-01 |
| out of memory in an allocation the runtime makes | the runtime pre-reserves its own structures; a failed task allocation returns `SpawnError::Alloc` | fault-injecting allocator test |
| process crash (kill -9, power loss) | acknowledged durable jobs survive; in-flight jobs are retried after their lease expires; no job runs twice under the same idempotency key inside the window | J-04, crash-at-every-step suite |
| a node is lost or partitioned (L3) | its leases expire; work is re-placed; fencing tokens reject late results from the old owner | Jepsen-style suite |
| shutdown with work in flight | `shutdown(deadline)` stops accepting work, lets running work finish until the deadline, then cancels the rest; when it returns no user code runs and all threads are joined | shutdown suite, E-11 |
| `panic = "abort"` in the user's profile | panic isolation is impossible by definition; the docs say so; supervision then restarts at process level (`service-kernel`) | documented, one test with abort |

---

## 8. Security responsibilities

- **Hostile input is the default.** Job payloads and cluster messages are untrusted bytes: size limits are checked before allocation, decoders are fuzzed, and decode errors are typed.
- **No code over the wire.** Remote spawn only runs registered, named, versioned task types. A peer cannot make a node run anything it did not register.
- **Authenticated peers only.** Cluster transport requires mutual authentication (`tls-net`). Membership changes are raft-committed.
- **Integrity at rest.** Durable job records carry a checksum; a corrupt record goes to the dead-letter queue with the reason, never runs. Encryption at rest is available through `crypt-io` for stores that hold sensitive payloads.
- **Isolation between tenants.** Per-tenant quotas (tasks, channels, jobs, CPU share through weighted lanes) so one tenant cannot starve another (Hive DB tenants, web stack users).
- **No denial of service through the scheduler.** Every limit in 5.16 exists to cap what one caller can consume.
- **Every security measure states its cost**, in latency and memory, measured; every performance shortcut states its security effect (from the stack-wide rule in `.claude/CLAUDE.md`).

---

## 9. Ecosystem and dependencies

### 9.1 task-io's own dependencies

None at runtime. Dev-only tooling is allowed: `criterion` (benches), `proptest` (property tests), `loom` (only under `cfg(loom)`), and the competitor crates used for comparison live in a separate, unpublished `compare/` crate, never in task-io's own dependency graph (D-010).

### 9.2 First-party crates task-io works with

| Crate | Relationship | When |
|---|---|---|
| `net-io` | implements task-io's `Driver` trait (task-io never depends on net-io) | 0.5.0 |
| `store-io` | backs the durable job store (O-6) | 0.4.0 |
| `raft-io`, `rpc-io`, `tls-net` | cluster membership, transport, authentication | 0.6.0 |
| `log-io`, `metrics-lib` | implement the `EventSink` trait | any |
| `service-kernel` | moves its supervised workers onto task-io | 0.5.0 |
| `spawn-io`, `actor-io` | built on task-io's monitors, deadlines, channels, placement, and job store | after 0.4.0 |
| `crypt-io` | encryption at rest for job stores | 0.4.0 |
| `clock-lib` | considered for the mockable clock in `sim` | 0.2.0 decides |

Any first-party dependency must be a published version, never a `path` dependency to a sibling repository (CI checks out each repository alone).

### 9.3 Relationship to `cycle`

`_strategy/CYCLE_STRATEGY.md` planned an async runtime as `cycle-*` components (`cycle-exec`, `cycle-time`, ...). None of them exist (`cycle-collection` is empty). task-io builds its executor and timer wheel itself and keeps them as separate internal modules with clean boundaries, so either can be extracted into a `cycle-*` crate later if a second consumer needs one without the rest (O-4).

### 9.4 Names

`task-forge` was planned as the in-process scheduler (`_strategy/LIBRARIES.md` 3.2). It was never built, and the name is taken on crates.io by another author (`task_forge`; crates.io treats `-` and `_` as the same). task-io covers in-process scheduling (D-002).

---

## 10. Phases

Summary only; exit criteria live in [`ROADMAP.md`](./ROADMAP.md).

| Version | Name | Delivers |
|---|---|---|
| 0.1.0 | Scaffold | repository, template, this plan, the performance contract, the R&D program |
| 0.2.0 | R&D and design lock | competitor harness and baseline numbers, experiments with verdicts, the spawn model, public API locked in `docs/API.md` |
| 0.3.0 | Core | tasks, scheduler, stealing, parking, topology, timers, cancellation, scopes, blocking handoff, local tasks, sync primitives, simulation mode |
| 0.4.0 | Jobs | durable jobs, delay, cron, retry, DLQ, idempotency, lanes, leases, stores |
| 0.5.0 | Integration | driver seam with net-io, Hive DB morsel scheduling, LexerSketch scheduler binding, service-kernel migration |
| 0.6.0 | Cluster | remote spawn, distributed jobs, partition testing |
| 0.7.0 | Advance | second R&D round; adopt what wins; all gates re-run |
| 0.9.0 | Hardening | fuzzing, 72-hour soak, chaos, adversarial review, audit |
| 1.0.0 | Stable | frozen after three consumers have run it in production |

---

## 11. Testing and verification

- **Unit and doc tests** for every public item, both `Ok` and every `Err` path. Naming: `test_<subject>_<condition>_<expected>`.
- **`loom` models** for every lock-free structure and every state-word transition (`RUSTFLAGS="--cfg loom"`).
- **Miri** for every module with `unsafe` (`cargo +nightly miri test`), including the slab, raw task pointers, and queues; Stacked and Tree Borrows both.
- **Sanitizers** on Linux nightly: ThreadSanitizer and AddressSanitizer on the stress suite.
- **Property tests** (`proptest`): queue operations against a model, timer wheel against a sorted-list model, scheduler state machine against a model, job state machine against a model.
- **Deterministic simulation:** thousands of seeds per CI run over the scheduler, monitors, and jobs; any failing seed becomes a permanent regression test.
- **Stress tests** on Linux, macOS, and Windows: spawn storms, steal storms, wake races, shutdown under load, all with leak checks (tasks, threads, memory return to baseline).
- **Fault injection:** a panicking future at every poll, a failing allocator, a store that fails or crashes at every step, a clock that jumps.
- **Crash tests:** kill the process at every step of the job state machine; verify no acknowledged job is lost and no idempotency key runs twice.
- **Fuzzing** for every decoder (job records, cluster messages).
- **Benchmarks:** the full contract in [`BENCHMARKS.md`](./BENCHMARKS.md), gated.
- **Edge cases tested, not assumed:** zero workers requested, one worker, more workers than cores, a limit of zero, a limit of `usize::MAX`, a future that is ready immediately, one that never completes, one that panics on drop, a waker cloned a million times, wake after completion, wake from a foreign thread, spawn during shutdown, drop of the runtime from inside a task, and recursive `block_on`.

---

## 12. Open decisions

These are James's calls. Each is written so it can be answered with one line.

| Id | Question | Recommendation |
|---|---|---|
| O-1 | ~~Describe your spawn model.~~ Answered 2026-10-13: written in `_strategy/SPAWN-IO.md`; it belongs to `spawn-io` (D-015). Confirm the list of what task-io provides for it (section 5.10). | confirm the list |
| O-2 | Publish `0.1.0` to crates.io now to claim the `task-io` name? | yes; the name is free today and the scaffold builds clean |
| O-3 | REPS says "MUST use tokio". task-io replaces tokio. Record task-io as an exception, or change the REPS rule portfolio-wide to "no rented runtime" (same question as net-io O-1)? | change REPS portfolio-wide |
| O-4 | Keep the executor and timer wheel inside task-io, with extraction into `cycle-*` crates only when a second consumer needs them alone? | yes |
| O-5 | If, after every experiment, one gated benchmark cannot reach the 1.20 factor, the release stays blocked. Confirm that only you can waive a single benchmark, in writing in section 14. | yes, keep the block |
| O-6 | Durable job store: use `store-io` (first-party, published version) behind the `job` feature, or ship a small internal log? | `store-io` once its Submitter ships in a published version; memory store until then |
| O-7 | Reference hardware for performance gates is this machine (Windows) and its WSL2 Ubuntu. Add a bare-metal Linux box and a Mac when available? | yes when available; until then Windows and WSL2 gate, macOS is reported only |

---

## 13. Ideas

The idea register, with hypothesis, method, cost, and status for each, lives in [`RESEARCH.md`](./RESEARCH.md) section 4. Ideas that are adopted move into section 5 here with a decision in section 14; ideas that are dropped stay in RESEARCH with the evidence.

---

## 14. Decision log

| Id | Date | Decision | Reason |
|---|---|---|---|
| D-001 | 2026-10-13 | Template aligned with net-io: edition 2024, MSRV 1.85, `Apache-2.0 OR MIT`, canonical `REPS.md`, `dev/` committed and `.dev/` private, no committed lockfile | portfolio consistency; the May scaffold (edition 2021, MSRV 1.75, Apache only, a stub REPS) was never published |
| D-002 | 2026-10-13 | Scope is a task runtime (local scheduler, jobs, then distribution), not only distributed dispatch (actors and spawns moved out by D-015) | James asked for workers, actors, and his spawn model here; the web stack needs local jobs now; Hive DB REQ-035 and LexerSketch need a scheduler; nothing else in the portfolio provides one (`cycle` is paused, `task-forge` was never built and its name is taken) |
| D-003 | 2026-10-13 | No tokio and no third-party runtime dependency; task-io is its own scheduler | ours, not rented; a runtime cannot beat tokio by depending on it (REPS exception pending O-3) |
| D-004 | 2026-10-13 | Sockets and the reactor stay in net-io; task-io defines a `Driver` trait that net-io implements; task-io never depends on net-io | one-way dependencies; the protocol-free and IO-free split matches net-io D-006 |
| D-005 | 2026-10-13 | One pool for async, blocking, CPU, local, and job work, with handoff for blocking work | one budget and fewer threads; to be confirmed or reverted by RESEARCH I-01 |
| D-006 | 2026-10-13 | Performance gate: primary metric at least 1.20 times the best competitor on every benchmark in BENCHMARKS.md, p99 included; a miss blocks the release; only James can waive | James's mandate: being slower than another library is failure |
| D-007 | 2026-10-13 | The spawn model is James's design; agents do not invent it (updated by D-015: James described it, and it moves to spawn-io) | the idea is his; a guessed version would be built on sand |
| D-008 | 2026-10-13 | Panics in user code are caught at the task boundary; the runtime and its workers never die from user code | stability mandate (4.2) |
| D-009 | 2026-10-13 | `unsafe` only in `raw/`, `queue/`, and `sys/`; until those exist the crate is `#![forbid(unsafe_code)]` | small, auditable unsafe surface; each module gets loom and Miri coverage |
| D-010 | 2026-10-13 | Competitor benchmarks live in `compare/`, a separate unpublished crate with its own `[workspace]`, never in task-io's dependency graph | keeps `cargo deny` and the published package clean of tokio, rayon, actix and the rest |
| D-011 | 2026-10-13 | Stability and safety mandates in section 4.2 are release blockers | James's mandate: fail-safe, error-hardened, no corners cut |
| D-012 | 2026-10-13 | API simplicity mandate in section 4.3: three-line Tier 1, one way to do each thing | James's mandate |
| D-013 | 2026-10-13 | Testing mandate in section 4.4 and section 11 | James's mandate: nothing untested, edge cases included |
| D-014 | 2026-10-13 | `0.2.0` is an R&D phase with a written program (RESEARCH.md) and an innovation requirement: at least three capabilities no benchmarked competitor has, each proven by experiment | James asked for invention, not a copy of existing runtimes |
| D-015 | 2026-10-13 | Actors move to `actor-io` and spawns to `spawn-io`, separate crates built on task-io; task-io keeps tasks, workers, jobs, and distribution, and adds monitors as the base for supervisors | James: task-io is a specific, single-purpose library; his spawn concept (`_strategy/SPAWN-IO.md`) is its own design; a smaller task-io ships sooner, and the web stack needs it soon |
