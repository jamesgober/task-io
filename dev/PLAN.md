# task-io - Project Plan

> The design, strategy, and decision record for `task-io`. This is a living document: any design decision, idea, or open question about task-io that comes up anywhere (chat, review, a sibling crate) is written here, in [`RESEARCH.md`](./RESEARCH.md), or in [`ROADMAP.md`](./ROADMAP.md) before it is acted on, so nothing lives only in someone's memory.
>
> **Read order:** [`../REPS.md`](../REPS.md) (portfolio standard) -> [`DIRECTIVES.md`](./DIRECTIVES.md) (engineering law for this crate) -> **this file** -> [`BENCHMARKS.md`](./BENCHMARKS.md) (the performance contract) -> [`RESEARCH.md`](./RESEARCH.md) (the R&D program) -> [`ROADMAP.md`](./ROADMAP.md) (phases and exit criteria).
>
> **Status:** scaffold, `0.1.0`. R&D and design lock is `0.2.0`. Sections 5 and 6 are the **planned** shape; `0.2.0` either confirms each part with measurements or replaces it, and records why in section 14.

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

task-io is the task dispatch pipeline. Code hands it a unit of work, a **job** (a registered handler name plus a payload), and task-io makes sure the job runs: on a worker, in priority order, when it is due, retried when it fails for a temporary reason, set aside with the reason when it fails for good, never run twice under the same idempotency key, kept across a crash when the store is durable, and run on another node when the pipeline spans a cluster.

**Workers** are task-io's own threads. They sit in place and pull jobs until there are none left. A worker is a thread that runs a handler, so a handler can do anything a thread can do (files, sockets, blocking calls) without special APIs.

**The controller** is what sets task-io apart. It watches every worker's health and performance and the load on the pipeline, and it acts: it adds workers when the load needs them and the hardware can use them, retires them when the load drops below a configurable threshold, and repairs or replaces a worker that is stuck, slow, or failing, before anyone notices. A worker that needs repair because of task-io itself should be close to never seen; the 7-day soak (BENCHMARKS S-02) holds that to zero.

It owns:

- the worker pool, how workers are placed, parked, and woken,
- the controller: health records, load signals, scaling, repair,
- bounded priority lanes, fair scheduling between lanes and tenants, back-pressure,
- delayed jobs, jobs at a time, and cron schedules,
- retries with backoff and jitter, temporary vs permanent failure, the dead-letter queue,
- idempotency keys, leases with fencing tokens,
- stores: in memory, and its own durable log store,
- job and worker events for logs, metrics, and supervisors,
- dispatch across nodes, behind transport and membership traits.

It is single-purpose and dependency-free. It is not a general async runtime, an actor system, or a spawn framework (section 3.2), and it never takes a runtime dependency (section 9).

It is built for enterprise, distributed, very long-running pipelines under heavy load: the goal is the fastest and most dependable job pipeline available in any language, measured, with the numbers published. Section 4 turns that into hard rules.

---

## 2. Where task-io sits

### 2.1 Who uses it

| Consumer | What it needs from task-io | Where it is written |
|---|---|---|
| Web stack (`serve-net`, `web-net`, `gate-net`) | background jobs: emails, cleanup, session-expiry sweeps; a local job queue and cron first, distribution later | `_strategy/WEB-STACK-DELIVERABLES.md` section 7 |
| `smtp-net` | queuing, retries with backoff, and dead letters for outgoing mail; smtp-net classifies a failure as temporary or permanent, and task-io acts on it | `net-collection/smtp-net/dev/PLAN.md` |
| distributed services | the replacement for a RabbitMQ-style task queue (`LIBRARIES.md` section 6 lists task-io as the in-ecosystem choice instead of "lapin + custom") | `_strategy/LIBRARIES.md` |
| `spawn-io` (planned) | a store it can use as a durable mission ledger; retries and dead letters for missions | `_strategy/SPAWN-IO.md` section 5 |
| `service-kernel` | recurring and background work for services without each one running its own threads | `service-kernel/src/worker` |

### 2.2 The pipeline

```
 submit / submit_after / cron                                     events -> EventSink
        |                                                               ^
        v                                                               |
  [ admission: size limit, idempotency key, quota ] --refused--> SubmitError
        |                                                               |
        v                                                               |
  [ store: memory | durable log ] <-------------------------------+    |
        |                                                         |    |
        +--> delayed? --> [ timer wheel ] --due--+                |    |
        |                                        v                |    |
        +--------------------------------> [ priority lanes, DRR ]    |
                                                 |                |    |
                                                 v                |    |
                                   [ workers: lease -> run handler ]--+
                                          ^            |
                     health records,      |            |   ok -> complete
                     load signals         |            |   temporary -> retry (backoff)
                                          |            |   permanent / exhausted -> DLQ
                              [ controller: scale up / scale down / repair ]
```

Every arrow is bounded. Every state change of a job goes through the store, so a durable store makes the whole pipeline durable. The controller never touches the job hot path: workers only write their own counters, and the controller reads them.

---

## 3. Scope

### 3.1 In scope

- A worker pool of OS threads with topology-aware placement and parking.
- The controller: per-worker health records, pipeline load signals, scaling between a floor and a cap with configurable thresholds, and repair (section 5.4).
- Isolated workers: handlers that must be killable run in child worker processes (section 5.5).
- Job registration by name; handlers are plain functions; payloads are bytes with a size limit.
- Submission: now, after a delay, at a time, on a cron schedule; `submit` waits when full, `try_submit` fails fast.
- Priority lanes and per-tenant quotas, scheduled by deficit round robin.
- Retries: exponential backoff with full jitter, a cap, a retry budget per handler, a circuit breaker per handler, and a classification of every failure as temporary or permanent.
- Dead-letter queue with the reason and attempt history; replay from the DLQ.
- Idempotency keys with a dedupe window.
- Leases (visibility timeouts) with fencing tokens.
- A `Store` trait, a memory store, and task-io's own durable log store (section 5.10).
- Job and worker events through an `EventSink` trait; counters and histograms.
- A deterministic simulation mode (seeded clock, worker interleaving, load patterns, and store faults).
- Seams for what task-io does not implement itself: `Executor` (async handlers), `Sealer` (encryption at rest), `Transport` and `Membership` (cluster).
- Dispatch across nodes: placement, leases fenced by term, failover, all in task-io behind the cluster traits (0.6.0).

### 3.2 Out of scope, and where it goes

| Not here | Where it lives | Why |
|---|---|---|
| A general async executor (spawning futures, work stealing of tasks, fork-join, async scopes, wakers) | the executor crate (candidate `cycle-exec`, O-1); design saved in `_strategy/EXECUTOR-SEED/` | different purpose (`LIBRARIES.md` 3.2) |
| Spawns: spawner, missions, mission ledger, replacement, elastic replication of missions | `spawn-io` (planned) | James's own design (`_strategy/SPAWN-IO.md`) |
| Actors, mailboxes, supervision trees | `actor-io` (planned, if needed) | a programming model, not dispatch |
| Encryption algorithms | `crypt-io`, through an adapter crate implementing `Sealer` | no home-made cryptography; no dependency in task-io |
| TLS, consensus, RPC framing | `tls-net`, `raft-io`, `rpc-io`, through an adapter crate implementing `Transport` and `Membership` | same |
| A general storage engine | `store-io` (optional adapter implementing `Store`) | task-io's own log store covers the job pipeline; store-io serves deployments that already run it |
| Logging and metrics backends | `log-io`, `metrics-lib` | they implement `EventSink` |

---

## 4. Mandates

These are requirements, not goals. A release that misses any of them does not ship. They came from James on 2026-10-13 and 2026-10-14 (D-006, D-011, D-012, D-013, D-019) and only James can change them, in writing, in section 14.

### 4.1 Performance

- **Faster than everything, by a margin.** On every benchmark in [`BENCHMARKS.md`](./BENCHMARKS.md), task-io's primary metric must beat the best competitor by a speedup factor of **at least 1.20** (20%), measured by the protocol in that file. The stretch target is 2.0. Losing to, or tying with, any competitor on any gated benchmark is a release blocker.
- **Lowest overhead.** Steady-state dispatch of a job with a small payload makes zero heap allocations. An idle pool makes zero wakeups (the controller included). Memory per queued job beats every competitor by the same 1.20 factor. The controller costs at most 1% of throughput (BENCHMARKS W-14).
- **No bottleneck by construction.** No global lock on submit, dispatch, or complete; no shared counter written per job; the controller is off the hot path; every structure scales with the number of workers and lanes.
- **Tail latency is a first-class metric.** p99 and p99.9 are gated, not only p50.
- **No regressions.** After a benchmark passes, a change that makes it more than 5% slower is rejected (REPS performance gate).

### 4.2 Stability and durability

- **The pool never dies because of a handler.** A panic in a handler is caught, classified as a failure, and retried or dead-lettered; the worker keeps running.
- **Self-managing.** The controller detects a stuck, slow, failing, or dead worker and repairs or replaces it without losing or duplicating a job, and reports what it did and why.
- **Nothing is unbounded.** Every lane, delayed set, dedupe index, store, DLQ, retry loop, and the worker count itself has a limit that is checked before resources are committed. Overload produces back-pressure or a typed rejection, never unbounded growth.
- **No lost jobs.** A job accepted by `submit` reaches exactly one end state (completed or dead-lettered), or stays pending. With the durable store, that holds across a crash or power loss at any instruction.
- **Built for months of uptime.** Zero drift in memory, threads, handles, and latency over a 7-day soak under heavy load (BENCHMARKS S-02).
- **Fail closed and fail visible.** Every error is typed, `#[non_exhaustive]`, and says what to do. Every failure of a job and every action of the controller is recorded with its reason. Nothing is dropped silently.
- **Proven, not hoped.** Every lock-free structure has a `loom` model; every `unsafe` block runs under Miri; every invariant in DIRECTIVES section 4 has a test that would fail if it broke.

### 4.3 Simplicity

- **The first screen of the docs is four lines of code:** create the pool, register a handler, submit a job, shut down. Correct defaults for every limit, the controller included: a user who never opens the builder gets a pool that sizes and repairs itself safely.
- **Handlers are plain functions.** No trait to implement for the common case.
- **One way to do each thing.** No two public functions with overlapping jobs.
- **Errors you can act on.** Each error variant names the limit or state that caused it.
- **Three tiers.** Tier 1 (the defaults) headlines the docs; Tier 2 (builders and knobs, controller thresholds included) is one click away; Tier 3 (the `Store`, `EventSink`, `Executor`, `Sealer`, `Transport`, `Membership` seams) is for crate authors.

### 4.4 Testing

- Every public item: unit tests for the `Ok` and every `Err` path, a runnable doc example.
- Every concurrent structure: `loom` model plus stress tests on all three operating systems.
- Every invariant: a property test or a deterministic-simulation test.
- Every failure mode in section 7: a test that injects it.
- Every controller decision: simulation tests over load patterns (steps, ramps, bursts, oscillating load) and fault patterns.
- Every store: the crash-at-every-step suite.
- Every claim in a doc: a test or benchmark that proves it. Full list in section 11.

### 4.5 Zero dependencies

task-io has **zero runtime dependencies, in every feature combination, permanently** (D-017). Not tokio, not serde, and not first-party crates either: anything task-io does not implement itself is a trait in task-io, implemented in a separate adapter crate that depends on task-io, never the reverse. Dev-only tooling is the only exception (section 9.1).

---

## 5. Architecture (planned)

This is the starting design. R&D in `0.2.0` ([`RESEARCH.md`](./RESEARCH.md)) measures it against alternatives; whatever wins is what gets built.

### 5.1 Module map (planned)

```
src/
  lib.rs          crate root, lints, Tier 1 re-exports
  error.rs        Error, SubmitError, Fail, ... (#[non_exhaustive])
  pool/           Pool, Builder, shutdown, worker threads, parking
  control/        the controller: health records, load signals, scaling, repair
  isolate/        isolated workers in child processes (framing, supervision, kill)
  job/            Job, JobId, registry, payload, context passed to handlers
  lane/           priority lanes, tenant quotas, deficit round robin, aging
  queue/          (unsafe) lock-free ring queues, eventcount
  time/           timer wheel for delayed jobs; cron parser and next-fire calculation
  retry/          backoff, jitter, retry budgets, circuit breakers, failure classification
  dlq/            dead-letter queue, replay
  dedupe/         idempotency index with a time window
  lease/          leases, fencing tokens
  store/          Store trait, memory store, durable log store, the crash-test harness
  event/          EventSink trait, job and worker events, counters, histograms
  seam/           Executor, Sealer, Transport, Membership traits
  sim/            deterministic simulation (feature `sim`)
  cluster/        (0.6.0) placement, remote leases, failover over the cluster traits
  sys/            (unsafe) OS calls std lacks: CPU topology, affinity, per-thread CPU time,
                  futex / WaitOnAddress, durable-write flags
```

`unsafe` is allowed only in `queue/` and `sys/` (DIRECTIVES section 2).

### 5.2 A job

- **Identity:** a `JobId` (unique within the store, never reused), the handler name, and an optional idempotency key.
- **Payload:** bytes, with a size limit checked at submit. Small payloads are stored inline in the job slot, so dispatching them allocates nothing (RESEARCH I-16).
- **Metadata:** lane, tenant, attempt count, not-before time, deadline, lease token.
- **State machine:** `Pending -> Leased -> (Completed | RetryWait -> Pending | DeadLettered)`. Every transition is one store operation. The memory store does it with one atomic write per transition on the hot path.

### 5.3 Workers

- One worker per physical core at start, placed by topology (core complexes and NUMA nodes on Linux, Windows, and macOS through `sys/`).
- A worker takes a **batch** of jobs per queue operation, not one, to cut contention (I-02).
- Idle workers park on an eventcount: no lost wakeups, no thundering herd, zero CPU when idle (I-04, I-05).
- Each worker owns one **health record**, written only by that worker with plain relaxed stores, cache-line padded so no two workers share a line: state, the start tick of the current job, jobs completed, failures, panics, busy time, and a heartbeat epoch. Writing it is a few stores per job, with no branch and no shared atomic (I-27).

### 5.4 The controller

One controller per pool. It is not on the job path: it reads health records and lane counters, decides, and acts.

**Signals**

| Signal | Source |
|---|---|
| backlog | lane depths and the age of the oldest ready job per lane |
| queue wait | sampled wait time (ready to start), p50 and p99 over a sliding window |
| arrival and completion rates | lane counters |
| busy vs blocked | per-worker busy time against per-thread CPU time from `sys/` (Linux `pthread_getcpuclockid`, Windows `GetThreadTimes`, macOS `thread_info`): a worker that is busy but uses little CPU is blocked on IO, and more workers help; a worker that is busy and CPU-bound gains nothing from more workers than cores (I-23) |
| worker latency | per-worker job time compared with its peers on the same handler (median absolute deviation) to find a slow worker (I-24) |
| stuck | a job running past its lease or deadline |
| failing | failure and panic rates per worker and per handler over a window |

**Scaling**

- **Up** when backlog or queue wait p99 stays above the target for the configured window **and** more workers would help (the pool is below the core count, or workers are mostly blocked). Never past the cap.
- **Down** when the idle fraction stays above the configured threshold for the cooldown window. Workers retire one at a time, finishing their current job first; never below the floor.
- **Stable:** hysteresis between the up and down thresholds, a cooldown after each change, and a cap on the rate of change, so the worker count never oscillates (BENCHMARKS W-12 counts reversals).
- The control law (additive increase and multiplicative decrease, a PID controller, or a queueing-model estimate) is chosen by R&D (I-21).

**Repair**

| Worker state | How it is detected | What the controller does |
|---|---|---|
| healthy | none of the below | nothing |
| slow | job time far above its peers on the same handler | stops giving it new batches, watches it; retires and replaces it if it stays slow |
| stuck | a job past its lease or deadline | expires the lease (the fencing token rejects the late result), starts a replacement worker at once, retires the stuck one: it takes no new job and exits when its handler returns |
| failing | failure rate above threshold | if the failures follow a handler, opens that handler's circuit breaker (the handler is the problem, not the worker); if they follow the worker, retires and replaces it |
| dead | heartbeat stops, or the thread ended outside shutdown (should never happen) | re-queues its leased jobs, starts a replacement, raises a critical event |

- Retired-but-stuck threads count against a **zombie cap**. When it is reached, the controller stops adding workers for the handler causing it, opens its circuit breaker, and raises a critical event, so a broken handler can never grow the thread count without limit.
- **A Rust thread cannot be killed safely.** A stuck handler on a thread is abandoned and fenced, never killed. Handlers that must be killable run as isolated workers (5.5).
- Every action is an event with the worker, the signal, the threshold, and the reason.

**The controller fails safe**

- With the pipeline idle, the controller parks with no timer; the first submit wakes it. Idle stays at zero wakeups.
- With jobs running, it samples on an interval (planned 10 ms, set by R&D), and a stuck-job deadline wakes it directly.
- If the controller itself panics, it is caught and restarted. The workers never depend on it to run jobs: without it the pool keeps working at its current size, and a worker restarts it when its heartbeat goes stale.

### 5.5 Isolated workers

A handler registered as `isolated` runs in a pool of child worker processes instead of threads. The cost is a process boundary per job (measured); the gain is that a stuck or crashing handler can be killed (`TerminateProcess`, `kill`) and replaced, and its crash cannot take the main process down. The application calls `task_io::worker_main()` at the start of `main` so the same binary can serve as the child. Framing over pipes is task-io's own, size-checked, and fuzzed. Zero dependencies: `std::process` only (I-25).

### 5.6 Lanes, fairness, back-pressure

- Each lane is a bounded queue. Lanes have weights. Workers pick lanes by deficit round robin, so a low-priority lane gets its share and is never starved, and aging lifts jobs that waited too long (I-08, I-19).
- Per-tenant quotas inside a lane: one tenant cannot fill the pipeline.
- Back-pressure: when a lane is full, `submit` waits (with a deadline) and `try_submit` returns `SubmitError::Full`.

### 5.7 Time: delays and cron

- A hierarchical timing wheel holds delayed jobs, with O(1) insert and cancel and no global lock. A job moves to its lane when it is due; it never runs early.
- Cron expressions are parsed once (fuzzed parser), and the next fire time is computed directly, so no schedule is scanned on each tick (I-17). Daylight-saving changes and leap days are handled and tested.
- Tickless: no wakeup when nothing is due.

### 5.8 Failure handling

- A handler returns `Ok(())` or a `Fail`: `Fail::retry(reason)` (temporary) or `Fail::permanent(reason)`. A panic counts as temporary by default (configurable per handler).
- Retry: exponential backoff with full jitter, capped, and a per-handler retry budget (a token bucket), so a broken downstream service cannot cause a retry storm (I-22).
- Circuit breaker per handler: after a failure threshold the handler's jobs wait instead of running, and a probe job tests recovery.
- Dead-letter queue: the job, every attempt's reason and time, and the final reason. Jobs can be inspected and replayed.

### 5.9 Idempotency and leases

- **Idempotency:** a job with a key is refused if the same key completed or is in flight within the dedupe window. The index is time-bucketed so memory is bounded and old buckets drop in O(1) (I-13).
- **Leases:** a worker holds a lease with a fencing token while it runs a job. A result is committed only if the token is still current. This is what makes repair, isolated-worker kills, and node failover safe (I-12).
- **Delivery guarantee, stated precisely:** at-least-once execution, and at-most-once effect per idempotency key within the window. Exactly-once is not claimed, because it cannot be guaranteed for effects outside the store.

### 5.10 Stores

- `Store` trait covering every job state transition, designed for group commit.
- **Memory store**: fastest; loses pending jobs on restart (documented).
- **Durable log store**, task-io's own (D-018): an append-only log of job records with a checksum on every record (CRC-32C, own implementation, hardware-accelerated where the CPU has it), group commit (one flush for a batch of submits), crash recovery by scanning from the last checkpoint, and compaction. Writes are flushed to stable storage before a submit is acknowledged (`sync_data`, plus the OS flags in `sys/` where a platform needs them). Zero dependencies (I-11, I-18).
- **Encryption at rest** through the `Sealer` trait (an adapter crate wires `crypt-io`). task-io never implements a cipher.
- Optional adapter: a `store-io`-backed store for deployments that already run store-io.
- Every store, including adapters, must pass the same crash-at-every-step and corruption suites before it ships.

### 5.11 Events and observability

Every job transition and every controller action emits an event to an `EventSink` (log-io and metrics-lib implement it; spawn-io can use it to watch missions). Per-worker counters with no shared atomics on the hot path; histograms merged on read. A health snapshot API returns the pool, lane, and worker state for dashboards and health checks. With no sink attached, the cost is one predictable branch (I-15).

### 5.12 Seams

| Trait | What it lets an adapter provide | task-io's default |
|---|---|---|
| `Store` | another storage backend | memory store; durable log store |
| `EventSink` | logs, metrics, tracing | none (one branch) |
| `Executor` | running async handlers on an external executor | none; handlers are sync |
| `Sealer` | encryption at rest | none (plaintext, checksummed) |
| `Transport` | authenticated, encrypted links between nodes | in-memory transport for tests and simulation only |
| `Membership` | cluster membership, terms, and leases from a consensus layer | single-node |

Each seam costs nothing when unused (I-20).

### 5.13 Deterministic simulation

A `sim` feature replaces the clock, the worker interleaving, the load, and store faults with a seeded, single-threaded version. The same seed replays the same run, so a bug found once can be replayed forever. task-io uses it to test the pipeline and the controller (load steps, bursts, stuck handlers, crashes at every step), and users can test their own handlers with it (I-14).

### 5.14 Dispatch across nodes (0.6.0)

- The logic is in task-io: placement by load and locality, remote leases fenced by the membership term, failover of a lost node's jobs, and the job wire format (size-checked, fuzzed).
- The links are not: `Transport` (an adapter over `net-io`, `rpc-io`, and `tls-net` with mutual authentication) and `Membership` (an adapter over `raft-io`).
- Only registered, named, versioned job types run on a remote node; no code crosses the wire.
- Each node runs its own controller; placement shifts load to nodes with spare capacity.
- Partitions, node loss, clock skew, message loss and duplication are tested with a Jepsen-style harness before the layer ships.

### 5.15 Limits and defaults (proposed, finalized at 0.2.0)

| Limit | Default | Guards against |
|---|---|---|
| workers at start | one per physical core | oversubscription |
| worker floor / cap | 1 / 8 per physical core (CPU-bound pools capped at logical cores) | starving a quiet pool; thread explosion |
| scale-down threshold | 50% idle for 30 s | holding threads nobody needs |
| controller cooldown | 5 s after a change | oscillation |
| zombie cap | 4 per pool | a broken handler growing threads without limit |
| lane capacity | 65,536 jobs per lane | memory exhaustion from a submit loop |
| payload size | 64 KiB | one job holding large memory |
| inline payload | 64 bytes | allocation on the hot path |
| delayed jobs | 1,048,576 | timer floods |
| retries | 10, backoff capped at 1 hour | retry storms |
| retry budget | 10% of a handler's recent attempts | storms against a failing downstream |
| dedupe window | 24 hours, bounded entries | duplicate effects; memory |
| lease | 30 s, renewed by long handlers | a stuck handler holding a job forever |
| DLQ | 1,048,576 entries, oldest evicted to the sink first | unbounded dead letters |

Every default is measured in `0.2.0`, and each one is written down in `docs/API.md` along with the reason for its value.

---

## 6. API shape (planned)

This is the planned shape. `docs/API.md` locks it at `0.2.0`.

**Tier 1.**

```rust
use std::time::Duration;
use task_io::{Fail, Job, Pool};

fn send_email(job: &Job) -> Result<(), Fail> {
    let msg = decode(job.payload()).map_err(Fail::permanent)?;  // bad input never retries
    smtp_send(&msg).map_err(Fail::retry)?;                        // network errors retry
    Ok(())
}

fn main() -> Result<(), task_io::Error> {
    let pool = Pool::new()?;                       // sizes and repairs itself, safe limits
    pool.register("send_email", send_email)?;
    pool.submit("send_email", b"...")?;            // waits when full
    pool.submit_after("send_email", Duration::from_secs(30), b"...")?;
    pool.cron("0 3 * * *", "sweep_sessions", b"")?;
    pool.shutdown(Duration::from_secs(10))?;       // finish or hand back, then stop
    Ok(())
}
```

**Tier 2.**

```rust
let pool = Pool::builder()
    .workers(2..=64)                       // floor and cap
    .scale_down_when_idle(0.5, Duration::from_secs(30))
    .lane("critical", 8)                   // weight 8
    .lane("bulk", 1)
    .durable("/var/lib/app/jobs")          // task-io's own durable log store
    .build()?;

pool.job("send_email", payload)
    .lane("critical")
    .key(order_id)                         // idempotency key
    .deadline(Duration::from_secs(5))
    .submit()?;

pool.register_isolated("render_pdf", render_pdf)?;   // runs in a killable child process

let health = pool.health();                // pool, lane, and worker state
```

**Questions 0.2.0 must answer with evidence, with the current recommendation:**

| Question | Recommendation | Deciding evidence |
|---|---|---|
| Is the main type `Pool`? | `Pool` (O-7) | reads as what it is |
| How long does `submit` wait when a lane is full? | up to the job's deadline (default 1 s), then `SubmitError::Full`; `try_submit` never waits | bounded, no reachable hang |
| Typed payloads? | bytes in Tier 1; an optional `Codec` trait in Tier 2 | zero-copy for bytes; no serde dependency |
| Is a panic temporary or permanent by default? | temporary, with the retry budget and circuit breaker as the brakes | most panics in handlers come from bad external state |
| Is the controller on by default? | yes, with the defaults in 5.15; `workers(n..=n)` fixes the size | James's requirement; W-12 proves it |
| Async handlers before the executor crate exists? | the `Executor` trait only; no built-in executor | single purpose |

---

## 7. Failure model

What can go wrong, and what task-io guarantees when it does.

| Failure | Guarantee | Test |
|---|---|---|
| a handler panics | caught; recorded as a failure with the payload; retried or dead-lettered; the worker keeps running | unit, W-09 |
| a handler hangs | its lease expires; the controller starts a replacement worker and retires the stuck one; the job is retried; the late result is rejected by the fencing token | W-10, W-13, lease suite |
| a worker is slow | the controller stops feeding it, then replaces it if it stays slow | W-13 |
| a handler keeps failing | its circuit breaker opens; its jobs wait instead of burning retries; a probe tests recovery | R-02, W-13 |
| a worker thread ends outside shutdown | its leased jobs are re-queued; a replacement starts; a critical event is raised | W-13 (injected) |
| the controller panics | it is caught and restarted; workers keep running jobs meanwhile | controller suite |
| a load spike | the controller adds workers when they help, up to the cap; back-pressure covers the rest | W-08, W-12 |
| a downstream service is down | temporary failures back off with jitter; the retry budget and breaker stop a storm; jobs end in the DLQ with every reason when retries run out | R-01, R-02 |
| submit loop / timer flood / one tenant floods | limits and quotas refuse or delay new work before memory is committed | W-07, W-08 |
| the same job is submitted twice with a key | the second is refused inside the window | K-01 |
| out of memory in an allocation the pool makes | pool structures are pre-reserved; a failed allocation returns a typed error; no partial job is stored | fault-injecting allocator test |
| an isolated worker crashes or is killed | the main process is unaffected; the job is retried; a new child starts | isolated suite |
| process crash (kill -9, power loss) | memory store: pending jobs are lost (documented). Durable store: every accepted job survives; leased jobs are retried after their lease expires; no idempotency key commits twice | P-02, crash-at-every-step suite |
| a corrupt record in the durable store | never runs; goes to the DLQ with the reason; recovery continues past it | corruption suite |
| a disk full or a failed write | the submit returns a typed error before acknowledging; nothing half-written is acknowledged | fault-injecting store test |
| a node is lost or partitioned (0.6.0) | its leases expire; jobs are re-placed; fencing tokens reject late results | Jepsen-style suite |
| shutdown with work in flight | `shutdown(deadline)` stops accepting jobs, lets running handlers finish until the deadline, returns unstarted jobs to the store, then stops; when it returns, every worker thread and child process is gone | shutdown suite |
| `panic = "abort"` in the user's profile | a handler panic ends the process (except in isolated workers); with the durable store, the job is retried after restart; the docs say so | one test with abort |

---

## 8. Security responsibilities

- **Hostile input is the default.** Payloads, cron expressions, durable records, isolated-worker frames, and cluster messages are untrusted: size limits before allocation, fuzzed parsers and decoders, typed errors.
- **No code over the wire.** Remote nodes run only registered, named, versioned job types.
- **Authenticated peers only.** The cluster layer refuses to start on a `Transport` that does not report mutual authentication; the in-memory transport is test-only and cannot be enabled outside tests and `sim`.
- **Integrity and confidentiality at rest.** Every durable record carries a checksum; a corrupt record never runs. Encryption at rest through `Sealer` for stores that hold sensitive payloads (emails, tokens).
- **Isolation.** Per-tenant quotas and weighted lanes so one tenant cannot starve another; isolated workers so one handler's crash cannot take the process down.
- **Least privilege for children.** Isolated workers inherit no handles beyond their pipes, and the docs show how to drop privileges per platform.
- **No secrets in events by default.** Events carry ids, names, and reasons, not payloads, unless a sink opts in.
- **Every security measure states its cost**, in latency and memory, measured; every performance shortcut states its security effect.

---

## 9. Ecosystem and dependencies

### 9.1 task-io's own dependencies

**None, in every feature combination** (4.5, D-017). The only additions ever allowed are dev-dependencies for testing: `criterion`, `proptest`, and `loom` (only under `[target.'cfg(loom)'.dev-dependencies]`). Competitor crates for comparison live in `compare/`, a separate unpublished crate with its own workspace, never in task-io's dependency graph (D-010). CI fails if `cargo tree -e normal` for task-io shows anything but task-io itself.

### 9.2 Adapter crates (separate repositories or crates, depending on task-io)

Names are James's call (O-8).

| Adapter | Implements | Wraps | When |
|---|---|---|---|
| encryption adapter | `Sealer` | `crypt-io` | 0.4.0 |
| store-io adapter | `Store` | `store-io` | after store-io publishes its Submitter |
| cluster adapter (`task-io-cluster`) | `Transport`, `Membership` | `net-io`, `rpc-io`, `tls-net`, `raft-io` | 0.6.0 |
| log and metrics sinks | `EventSink` | `log-io`, `metrics-lib` (in those crates or an adapter) | any |
| executor adapter | `Executor` | the executor crate (O-1) | when it exists |

Each adapter passes the trait's conformance suite, which task-io publishes as a test module (`task_io::store::conformance`, and the same for each trait).

### 9.3 Crates that use task-io

| Crate | Uses | When |
|---|---|---|
| web stack | jobs, cron, durable store | 0.5.0 |
| `smtp-net` | jobs with failure classification | 0.5.0 |
| `service-kernel` | recurring service work | 0.5.0 |
| `spawn-io` | the store and events as its mission ledger | after 0.4.0 |

### 9.4 Names

`task-forge` was planned as the in-process scheduler (`LIBRARIES.md` 3.2). It was never built, and the name is taken on crates.io by another author (`task_forge`; crates.io treats `-` and `_` as the same). That role is now the executor crate (O-1).

---

## 10. Phases

Summary only; exit criteria live in [`ROADMAP.md`](./ROADMAP.md).

| Version | Name | Delivers |
|---|---|---|
| 0.1.0 | Scaffold | repository, template, this plan, the performance contract, the R&D program |
| 0.2.0 | R&D and design lock | competitor harness and baselines, experiments with verdicts, public API locked in `docs/API.md` |
| 0.3.0 | Local pipeline | workers, the controller (scaling and repair), lanes, submit, back-pressure, delays, cron, retries, breakers, DLQ, idempotency, leases, memory store, events, simulation |
| 0.4.0 | Durable | the durable log store, group commit, checksums, recovery, the `Sealer` seam, crash-at-every-step |
| 0.5.0 | Isolation and integration | isolated workers, the `Executor` seam, web stack, smtp-net, service-kernel |
| 0.6.0 | Cluster | dispatch across nodes over the `Transport` and `Membership` seams, the cluster adapter, partition testing |
| 0.7.0 | Advance | second R&D round; adopt what wins; all gates re-run |
| 0.9.0 | Hardening | fuzzing, the 7-day soak, chaos, adversarial review, audit |
| 1.0.0 | Stable | frozen after three consumers have run it in production |

---

## 11. Testing and verification

- **Unit and doc tests** for every public item, both `Ok` and every `Err` path. Naming: `test_<subject>_<condition>_<expected>`.
- **`loom` models** for every lock-free structure, every job state transition in the memory store, and the health-record protocol between workers and the controller.
- **Miri** for every module with `unsafe`.
- **Sanitizers** on Linux nightly: ThreadSanitizer and AddressSanitizer on the stress suite.
- **Property tests** (`proptest`): lanes against a model, DRR shares against the weights, the timer wheel against a sorted-list model, cron next-fire against a brute-force model (DST and leap days included), the job state machine against a model, the dedupe window against a model, the controller never leaving its floor and cap and never exceeding its change rate.
- **Deterministic simulation:** thousands of seeds per CI run over workers, the controller, lanes, retries, leases, and store faults, with load steps, ramps, bursts, and oscillating load; any failing seed becomes a permanent regression test.
- **Stress tests** on Linux, macOS, and Windows: submit storms, panic storms, stuck handlers, slow workers, shutdown under load, with leak checks (threads, handles, child processes, and memory return to baseline).
- **Fault injection:** a handler that panics at every point, a handler that hangs, a worker slowed on purpose, a failing allocator, a store that fails or crashes at every step, a full disk, a clock that jumps forward and backward, a killed isolated worker.
- **Crash tests:** kill the process at every step of the job state machine for every durable store; 0 lost jobs, 0 duplicate effects per key; power-loss simulation by truncating and corrupting the log at every byte of the last record.
- **Fuzzing:** the cron parser, the payload codec, durable record decoding, isolated-worker frames, cluster messages.
- **Soaks:** 24 hours in CI-adjacent runs; 7 days before 1.0 (BENCHMARKS S-02).
- **Benchmarks:** the full contract in [`BENCHMARKS.md`](./BENCHMARKS.md), gated.
- **Dependency check:** CI asserts that task-io's normal dependency tree is empty.
- **Edge cases tested, not assumed:** zero workers requested, a floor above the cap, one worker, more workers than cores, a limit of zero, a limit of `usize::MAX`, an empty payload, a payload of exactly the limit and one byte over, a handler that is not registered, a cron expression that never fires, a delay of zero and of `Duration::MAX`, a clock moved backward, shutdown during submit, submit during shutdown, a handler that submits jobs, a handler that calls `shutdown`, a handler that blocks forever during shutdown, the same key submitted from 32 threads at once, a load that rises and falls at exactly the scaling thresholds, and every worker stuck at once.

---

## 12. Open decisions

These are James's calls. Each is written so it can be answered with one line.

| Id | Question | Recommendation |
|---|---|---|
| O-1 | The general-purpose executor moved out of task-io: name it, and when to start it? The design is saved in `_strategy/EXECUTOR-SEED/`. | `cycle-exec`, as `CYCLE_STRATEGY.md` already plans; start when Hive DB's executor or LexerSketch needs it |
| O-2 | Publish `0.1.0` to crates.io now to claim the `task-io` name? | yes; the name is free today and the scaffold builds clean |
| O-3 | ~~REPS "MUST use tokio"~~ Answered 2026-10-14: the rule does not apply to task-io (D-016). | |
| O-4 | If, after every experiment, one gated benchmark cannot reach the 1.20 factor, the release stays blocked. Confirm that only you can waive a single benchmark, in writing in section 14. | yes, keep the block |
| O-5 | ~~Durable store through store-io or an internal log?~~ Decided 2026-10-14: task-io's own durable log store, store-io as an optional adapter (D-018). | |
| O-6 | Reference hardware for the gates is this machine (Windows) and its WSL2 Ubuntu. Add a bare-metal Linux box and a Mac when available? | yes when available; until then Windows and WSL2 gate, macOS is reported only |
| O-7 | Name of the main type: `Pool`? | `Pool` |
| O-8 | Names of the adapter crates. Cluster: answered 2026-10-14, `task-io-cluster` (D-021). Encryption and store-io: still open. | `task-io-crypt` (names what it plugs in, `crypt-io`), `task-io-store-io` (task-io already has its own stores, so `task-io-store` would mislead) |
| O-9 | `background-jobs` (a listed competitor) is AGPL-3.0. Add it as a dependency of the `compare/` harness in this MIT/Apache repository, benchmark it from a separate private harness, or drop it from the competitor set? | benchmark it from a separate, private harness that is never published, so no AGPL code enters this repository's dependency graph; the competitor set is never shrunk otherwise |
| O-10 | Several W-03 rows have competitor overhead under 0.1% (for example 0.016% for 1 ms jobs on 1 worker). A 1.20 factor there is a difference of a few thousandths of a percent, inside run-to-run noise, so no run can prove it. Keep the 1.20 gate on those rows anyway, or gate rows whose best overhead is under 0.1% at 1.0 (no worse than the best)? | gate them at 1.0 and say so in BENCHMARKS; every other W-03 row keeps 1.20. Only you can make this change (D-006) |

---

## 13. Ideas

The idea register, with hypothesis, method, cost, and status for each, lives in [`RESEARCH.md`](./RESEARCH.md) section 4. Adopted ideas move into section 5 with a decision in section 14; dropped ideas stay in RESEARCH with the evidence.

---

## 14. Decision log

| Id | Date | Decision | Reason |
|---|---|---|---|
| D-001 | 2026-10-13 | Template aligned with net-io: edition 2024, MSRV 1.85, `Apache-2.0 OR MIT`, canonical `REPS.md`, `dev/` committed and `.dev/` private, no committed lockfile | portfolio consistency; the May scaffold (edition 2021, MSRV 1.75, Apache only, a stub REPS) was never published |
| D-002 | 2026-10-13 | Scope is the task dispatch pipeline: workers, lanes, delays, cron, retries, DLQ, idempotency, leases, stores, then dispatch across nodes. Local first, distributed second | the original purpose (`LIBRARIES.md`: "distributed task dispatch", replacing "lapin + custom"); the web stack needs a local job queue and cron first (`WEB-STACK-DELIVERABLES.md` 7); distribution needs a correct local pipeline under it |
| D-003 | 2026-10-13 | No tokio and no runtime dependency; workers are task-io's own threads | ours, not rented; a pipeline cannot beat the competitors by depending on them |
| D-004 | 2026-10-13 | Handlers are plain functions running on worker threads; async handlers go through an `Executor` seam, never a bundled executor | single purpose; any handler can do blocking work safely |
| D-005 | 2026-10-13 | Every job state change goes through the `Store` trait; the memory store is the default | one code path for memory and durable stores; crash guarantees come from the store |
| D-006 | 2026-10-13 | Performance gate: primary metric at least 1.20 times the best competitor on every benchmark in BENCHMARKS.md, p99 included; a miss blocks the release; only James can waive | James's mandate: being slower than another library is failure |
| D-007 | 2026-10-13 | Delivery: at-least-once execution, at-most-once effect per idempotency key within the window; exactly-once is never claimed | it cannot be guaranteed for effects outside the store; claiming it would be false |
| D-008 | 2026-10-13 | Handler panics are caught and treated as failures; workers never die from handler code | stability mandate (4.2) |
| D-009 | 2026-10-13 | `unsafe` only in `queue/` and `sys/`; until those exist the crate is `#![forbid(unsafe_code)]` | small, auditable unsafe surface; loom and Miri coverage |
| D-010 | 2026-10-13 | Competitor benchmarks live in `compare/`, a separate unpublished crate with its own `[workspace]`, never in task-io's dependency graph | keeps `cargo deny` and the published package clean |
| D-011 | 2026-10-13 | Stability and safety mandates in section 4.2 are release blockers | James's mandate: fail-safe, error-hardened, no corners cut |
| D-012 | 2026-10-13 | API simplicity mandate in section 4.3 | James's mandate |
| D-013 | 2026-10-13 | Testing mandate in sections 4.4 and 11 | James's mandate: nothing untested, edge cases included |
| D-014 | 2026-10-13 | `0.2.0` is an R&D phase with a written program (RESEARCH.md) and an innovation requirement: at least three capabilities no benchmarked competitor has, each proven by experiment | James asked for invention, not a copy of existing designs |
| D-015 | 2026-10-13 | task-io is single-purpose. The first draft of this plan (commit 68bbce3) widened it into a full runtime with an executor, actors, and a slot for James's spawn model; that is reverted. The executor design moves to a separate crate (O-1) and is saved in `_strategy/EXECUTOR-SEED/`; actors move to `actor-io`; spawns move to `spawn-io` (`_strategy/SPAWN-IO.md`) | James: the libraries are meant to be single-purpose; `LIBRARIES.md` 3.2 separates in-process scheduling from dispatch; a smaller task-io ships sooner, and the web stack needs it soon |
| D-016 | 2026-10-14 | REPS's tokio rules ("MUST use tokio", `spawn_blocking`, `tokio::sync::Semaphore`) do not apply to task-io. Their intent is kept with task-io's own means: bounded concurrency (lanes, quotas, the worker cap), never blocking a shared executor (handlers run on task-io's threads), cancellation (deadlines, leases, shutdown) | James (REPS's author) says the tokio rule was added without his approval and is not his policy; task-io needs no async runtime anywhere in its design, so this is only a REPS wording issue, not a design conflict |
| D-017 | 2026-10-14 | Zero runtime dependencies in every feature combination, permanently, first-party crates included. Every integration is a trait in task-io implemented by an adapter crate that depends on task-io. CI checks the dependency tree is empty | the portfolio's dependency disposition (`LIBRARIES.md` 4.2) and the io crates' shared identity; the earlier draft listed store-io, crypt-io, raft-io, rpc-io, and tls-net as dependencies, which also pulled third-party crates in through crypt-io and rpc-io |
| D-018 | 2026-10-14 | task-io ships its own durable log store (append-only, checksummed, group commit, crash recovery); store-io becomes an optional adapter | durability is task-io's core promise and must not wait on another crate or add a dependency (D-017) |
| D-019 | 2026-10-14 | The controller is a core part of task-io from 0.3.0: health records, load signals, scaling between a floor and a cap with configurable thresholds, repair of stuck, slow, failing, and dead workers, on by default | James: the pool must monitor every worker and the pipeline load, spawn and retire workers, and repair failing ones, for enterprise, distributed, long-running pipelines under heavy load |
| D-020 | 2026-10-14 | Handlers that must be killable run as isolated workers in child processes | a Rust thread cannot be killed safely; process isolation is the only true kill, and it keeps a crashing handler from taking the process down |
| D-021 | 2026-10-14 | The cluster adapter crate is named `task-io-cluster` | James's choice (O-8); it bundles four crates (net-io, rpc-io, tls-net, raft-io) for one purpose, so it is named for the purpose |
| D-022 | 2026-10-19 | The `Store` contract requires one-step conditional transitions, one-step keyed puts, and a typed stale-token outcome; no store result ends a worker (`docs/ARCHITECTURE.md` 8.1) | fang 0.11 on SQLite showed each failure the contract rules out: jobs run twice from a read-then-write claim, repeated keys stored twice from a lookup-then-insert, and workers leaving the pool on a lost race (RESEARCH I-39, I-40) |
| D-023 | 2026-10-19 | `std` first: every OS operation `std` provides safely (flushing, killing a child, the CPU count under quotas and affinity, raw thread and process handles, parking) comes from `std`; `sys/` declares only what `std` lacks (topology, placement, per-thread CPU time, the high-resolution timer, child cleanup), and a lower-level park or wake replaces `std::thread::park` only after I-04 or I-34 measures a gain. The planned list is `docs/ARCHITECTURE.md` sections 11 and 12 | fewer `unsafe` blocks to prove (loom, Miri) and fewer platform contracts to track; `std`'s own implementations already use the right primitive on each OS (for example `F_FULLFSYNC` on macOS, read in the std source) |
| D-024 | 2026-10-19 | The `Store` trait takes transitions in batches (`apply`) plus a separate one-step `lease`; tokens are durable and only grow across restarts; an attempt is counted at lease; store times are wall-clock milliseconds; every store passes the conformance suite in `docs/ARCHITECTURE.md` 8.3, which ships in the library with no dependency (draft for the design lock) | batches give every store group commit for free; durable, growing tokens keep fencing correct through a crash; counting at lease stops a job that crashes or hangs its worker from retrying forever; each conformance check answers a failure found in a competitor (fang, effectum) or a mandate (bounded, hostile input) |
