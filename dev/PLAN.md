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

task-io is the task dispatch pipeline. Code hands it a unit of work, a **job** (a registered handler name plus a payload), and task-io makes sure the job runs: on a worker, in priority order, when it is due, retried when it fails for a temporary reason, set aside with the reason when it fails for good, never run twice under the same idempotency key, kept across a crash when the store is durable, and, later, run on another node.

**Workers** are task-io's own threads. They sit in place and pull jobs until there are none left. They are simple on purpose: a worker is a thread that runs a handler, so a handler can do anything a thread can do (files, sockets, blocking calls) without special APIs.

It owns:

- the worker pool and how workers are placed, parked, and woken,
- bounded priority lanes, fair scheduling between lanes and tenants, back-pressure,
- delayed jobs, jobs at a time, and cron schedules,
- retries with backoff and jitter, temporary vs permanent failure, the dead-letter queue,
- idempotency keys, leases with fencing tokens,
- the store: in memory, or durable through a pluggable store,
- job events (enqueued, started, completed, retried, dead-lettered) for logs, metrics, and supervisors,
- dispatch across nodes (later phase).

It is single-purpose. It is not a general async runtime, an actor system, or a spawn framework (section 3.2).

The goal: the fastest and most dependable job pipeline available in any language, measured, with the numbers published. Section 4 turns that into hard rules.

---

## 2. Where task-io sits

### 2.1 Who uses it

| Consumer | What it needs from task-io | Where it is written |
|---|---|---|
| Web stack (`serve-net`, `web-net`, `gate-net`) | background jobs: emails, cleanup, session-expiry sweeps; a local job queue and cron first, distribution later | `_strategy/WEB-STACK-DELIVERABLES.md` section 7 |
| `smtp-net` | queuing, retries with backoff, and dead letters for outgoing mail; smtp-net classifies a failure as temporary or permanent, and task-io acts on it | `net-collection/smtp-net/dev/PLAN.md` |
| `spawn-io` (planned) | a store it can use as a durable mission ledger; retries and dead letters for missions | `_strategy/SPAWN-IO.md` section 5 |
| `service-kernel` | recurring and background work for services without each one running its own threads | `service-kernel/src/worker` |

### 2.2 The pipeline

```
 submit / submit_after / cron                                         events -> EventSink
        |                                                                   ^
        v                                                                   |
  [ admission: size limit, idempotency key, quota ] --refused--> SubmitError |
        |                                                                   |
        v                                                                   |
  [ store: memory | durable ] <-----------------------------------+        |
        |                                                         |        |
        +--> delayed? --> [ timer wheel ] --due--+                |        |
        |                                        v                |        |
        +--------------------------------> [ priority lanes, DRR ]        |
                                                 |                |        |
                                                 v                |        |
                                   [ workers: lease -> run handler ]------+
                                                 |
                       ok -> complete | temporary -> retry (backoff) | permanent / exhausted -> DLQ
```

Every arrow is bounded. Every state change of a job goes through the store, so a durable store makes the whole pipeline durable.

---

## 3. Scope

### 3.1 In scope

- A worker pool of OS threads with topology-aware placement, parking, and an elastic size (RESEARCH I-21).
- Job registration by name; handlers are plain functions; payloads are bytes with a size limit.
- Submission: now, after a delay, at a time, on a cron schedule; `submit` waits when full, `try_submit` fails fast.
- Priority lanes and per-tenant quotas, scheduled by deficit round robin.
- Retries: exponential backoff with full jitter, a cap, a retry budget per handler, and a classification of every failure as temporary or permanent.
- Dead-letter queue with the reason and attempt history; replay from the DLQ.
- Idempotency keys with a dedupe window.
- Leases (visibility timeouts) with fencing tokens; work rescue for a handler that is stuck.
- A `Store` trait; a memory store in the crate; a durable store through a first-party storage crate (O-5).
- Job events through an `EventSink` trait; counters and histograms.
- A deterministic simulation mode (seeded clock, worker interleaving, and store faults).
- An `Executor` seam so async handlers can run on an external executor (no dependency).
- Dispatch across nodes (0.6.0).

### 3.2 Out of scope, and where it goes

| Not here | Where it lives | Why |
|---|---|---|
| A general async executor (spawning futures, work stealing of tasks, fork-join, async scopes, wakers) | the executor crate (candidate `cycle-exec`, O-1); the design is saved in `_strategy/EXECUTOR-SEED/` | different purpose (`LIBRARIES.md` 3.2): in-process scheduling vs dispatch |
| Spawns: spawner, missions, mission ledger, replacement, elastic replication | `spawn-io` (planned) | James's own design (`_strategy/SPAWN-IO.md`) |
| Actors, mailboxes, supervision trees | `actor-io` (planned, if needed) | a programming model, not dispatch |
| Sockets and the reactor | `net-io` | not dispatch |
| A storage engine | `store-io` (durable store) | task-io defines the `Store` trait and tests every implementation; it does not write a storage engine |
| Consensus, membership, transport between nodes | `raft-io`, `rpc-io`, `tls-net` | the cluster layer uses them through traits |
| Logging and metrics backends | `log-io`, `metrics-lib` | they implement `EventSink` |

---

## 4. Mandates

These are requirements, not goals. A release that misses any of them does not ship. They came from James on 2026-10-13 (D-006, D-011, D-012, D-013) and only James can change them, in writing, in section 14.

### 4.1 Performance

- **Faster than everything, by a margin.** On every benchmark in [`BENCHMARKS.md`](./BENCHMARKS.md), task-io's primary metric must beat the best competitor by a speedup factor of **at least 1.20** (20%), measured by the protocol in that file. The stretch target is 2.0. Losing to, or tying with, any competitor on any gated benchmark is a release blocker.
- **Lowest overhead.** Steady-state dispatch of a job with a small payload makes zero heap allocations. An idle pool makes zero wakeups. Memory per queued job beats every competitor by the same 1.20 factor.
- **Tail latency is a first-class metric.** p99 and p99.9 are gated, not only p50.
- **No regressions.** After a benchmark passes, a change that makes it more than 5% slower is rejected (REPS performance gate).

### 4.2 Stability and safety

- **The pool never dies because of a handler.** A panic in a handler is caught, classified as a failure, and retried or dead-lettered; the worker keeps running.
- **Nothing is unbounded.** Every lane, delayed set, dedupe index, store, and retry loop has a limit that is checked before memory is committed. Overload produces back-pressure or a typed rejection, never unbounded growth.
- **No lost jobs.** A job accepted by `submit` reaches exactly one end state (completed or dead-lettered), or stays pending. With a durable store, that holds across a crash at any instruction.
- **Fail closed and fail visible.** Every error is typed, `#[non_exhaustive]`, and says what to do. Every failure of a job is recorded with its reason. Nothing is dropped silently.
- **Proven, not hoped.** Every lock-free structure has a `loom` model; every `unsafe` block runs under Miri; every invariant in DIRECTIVES section 4 has a test that would fail if it broke.

### 4.3 Simplicity

- **The first screen of the docs is four lines of code:** create the pool, register a handler, submit a job, shut down. Correct defaults for every limit.
- **Handlers are plain functions.** No trait to implement for the common case.
- **One way to do each thing.** No two public functions with overlapping jobs.
- **Errors you can act on.** Each error variant names the limit or state that caused it.
- **Three tiers.** Tier 1 (the defaults) headlines the docs; Tier 2 (builders and knobs) is one click away; Tier 3 (`Store`, `EventSink`, `Executor` seams) is for crate authors.

### 4.4 Testing

- Every public item: unit tests for the `Ok` and every `Err` path, a runnable doc example.
- Every concurrent structure: `loom` model plus stress tests on all three operating systems.
- Every invariant: a property test or a deterministic-simulation test.
- Every failure mode in section 7: a test that injects it.
- Every store: the crash-at-every-step suite.
- Every claim in a doc: a test or benchmark that proves it. Full list in section 11.

---

## 5. Architecture (planned)

This is the starting design. R&D in `0.2.0` ([`RESEARCH.md`](./RESEARCH.md)) measures it against alternatives; whatever wins is what gets built.

### 5.1 Module map (planned)

```
src/
  lib.rs          crate root, lints, Tier 1 re-exports
  error.rs        Error, SubmitError, Fail, ... (#[non_exhaustive])
  pool/           Pool, Builder, shutdown, worker threads, parking, elastic sizing
  job/            Job, JobId, registry, payload, context passed to handlers
  lane/           priority lanes, tenant quotas, deficit round robin
  queue/          (unsafe) lock-free ring queues, eventcount
  time/           timer wheel for delayed jobs; cron parser and next-fire calculation
  retry/          backoff, jitter, retry budgets, failure classification
  dlq/            dead-letter queue, replay
  dedupe/         idempotency index with a time window
  lease/          leases, fencing tokens, work rescue
  store/          Store trait, memory store, the crash-test harness for implementations
  event/          EventSink trait, job events, counters, histograms
  exec/           Executor seam for async handlers
  sim/            deterministic simulation (feature `sim`)
  cluster/        (0.6.0; feature or adapter crate) dispatch across nodes
  sys/            (unsafe) OS calls std lacks: CPU topology, affinity, futex / WaitOnAddress
```

`unsafe` is allowed only in `queue/` and `sys/` (DIRECTIVES section 2).

### 5.2 A job

- **Identity:** a `JobId` (unique within the store, never reused), the handler name, and an optional idempotency key.
- **Payload:** bytes, with a size limit checked at submit. Small payloads are stored inline in the job slot, so dispatching them allocates nothing (RESEARCH I-16).
- **Metadata:** lane, tenant, attempt count, not-before time, deadline, lease token.
- **State machine:** `Pending -> Leased -> (Completed | RetryWait -> Pending | DeadLettered)`. Every transition is one store operation. The memory store does it with one atomic write per transition on the hot path.

### 5.3 Workers

- One worker per physical core by default, placed by topology (core complexes and NUMA nodes on Linux, Windows, and macOS through `sys/`).
- A worker takes a **batch** of jobs per queue operation, not one, to cut contention (RESEARCH I-02).
- Idle workers park on an eventcount: no lost wakeups, no thundering herd, zero CPU when idle (I-04, I-05).
- **Elastic size:** the pool adds workers while throughput rises and removes them when it stops rising, between a floor and a cap (I-21, from the elastic-replication idea in `_strategy/SPAWN-IO.md` 4.7). Off by default until measurement proves it.
- **Work rescue:** a watchdog sees a handler that runs past its deadline. Its lease expires, a spare worker takes over its lane, and the job is retried. When the stuck handler finally returns, its result is rejected by the fencing token (I-10).

### 5.4 Lanes, fairness, back-pressure

- Each lane is a bounded queue. Lanes have weights. Workers pick lanes by deficit round robin, so a low-priority lane gets its share and is never starved, and aging lifts jobs that waited too long (I-19).
- Per-tenant quotas inside a lane: one tenant cannot fill the pipeline.
- Back-pressure: when a lane is full, `submit` waits (with a deadline) and `try_submit` returns `SubmitError::Full`.

### 5.5 Time: delays and cron

- A hierarchical timing wheel holds delayed jobs, with O(1) insert and cancel and no global lock. A job moves to its lane when it is due; it never runs early.
- Cron expressions are parsed once (fuzzed parser), and the next fire time is computed directly, so no schedule is scanned on each tick (I-17).
- Tickless: no wakeup when nothing is due.

### 5.6 Failure handling

- A handler returns `Ok(())` or a `Fail`: `Fail::retry(reason)` (temporary) or `Fail::permanent(reason)`. A panic counts as temporary by default (configurable per handler).
- Retry: exponential backoff with full jitter, capped, and a per-handler retry budget (a token bucket), so a broken downstream service cannot cause a retry storm (I-22).
- Dead-letter queue: the job, every attempt's reason and time, and the final reason. Jobs can be inspected and replayed.

### 5.7 Idempotency and leases

- **Idempotency:** a job with a key is refused if the same key completed or is in flight within the dedupe window. The index is time-bucketed so memory is bounded and old buckets drop in O(1) (I-13).
- **Leases:** a worker holds a lease with a fencing token while it runs a job. A result is committed only if the token is still current. This is what makes work rescue and, later, node failover safe (I-12).
- **Delivery guarantee, stated precisely:** at-least-once execution, and at-most-once effect per idempotency key within the window. Exactly-once is not claimed, because it cannot be guaranteed for effects outside the store.

### 5.8 Stores

- `Store` trait covering every job state transition, designed so a durable implementation can group-commit (I-11).
- Memory store in the crate: fastest, loses pending jobs on restart (documented).
- Durable store through a first-party storage crate (O-5), with record checksums and optional encryption through `crypt-io`.
- Every implementation must pass the same crash-at-every-step suite before it ships.

### 5.9 Events and observability

Every transition emits a job event to an `EventSink` (log-io and metrics-lib implement it; spawn-io can use it to watch missions). Per-worker counters with no shared atomics on the hot path; histograms merged on read. With no sink attached, the cost is one predictable branch (I-15).

### 5.10 Executor seam

Handlers are synchronous by default and run on task-io's workers. An `Executor` trait lets an async handler run on an external executor (the future executor crate, O-1, or any other). task-io never depends on an executor, and the seam costs nothing when unused (I-20).

### 5.11 Deterministic simulation

A `sim` feature replaces the clock, the worker interleaving, and store faults with a seeded, single-threaded version. The same seed replays the same run, so a bug found once can be replayed forever. task-io tests itself with it, and users can test their own handlers with it (I-14).

### 5.12 Dispatch across nodes (0.6.0)

- Membership and leases through `raft-io`; transport through `rpc-io` over `net-io` with `tls-net` mutual authentication; all behind traits.
- Only registered, named, versioned job types run on a remote node; no code crosses the wire.
- Leases fenced by the raft term, so a partitioned node's late result is rejected.
- Placement by load and locality. Partitions, node loss, clock skew, message loss and duplication are tested with a Jepsen-style harness before the layer ships.

### 5.13 Limits and defaults (proposed, finalized at 0.2.0)

| Limit | Default | Guards against |
|---|---|---|
| workers | one per physical core | oversubscription |
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
    let pool = Pool::new()?;                                  // one worker per core, safe limits
    pool.register("send_email", send_email)?;
    pool.submit("send_email", b"...")?;                       // waits when full
    pool.submit_after("send_email", Duration::from_secs(30), b"...")?;
    pool.cron("0 3 * * *", "sweep_sessions", b"")?;
    pool.shutdown(Duration::from_secs(10))?;                  // finish or hand back, then stop
    Ok(())
}
```

**Tier 2.**

```rust
let pool = Pool::builder()
    .workers(8)
    .lane("critical", 8)        // weight 8
    .lane("bulk", 1)
    .store(my_durable_store)
    .build()?;

pool.job("send_email", payload)
    .lane("critical")
    .key(order_id)              // idempotency key
    .deadline(Duration::from_secs(5))
    .submit()?;
```

**Questions 0.2.0 must answer with evidence, with the current recommendation:**

| Question | Recommendation | Deciding evidence |
|---|---|---|
| Is the main type `Pool`? | `Pool` (O-7) | reads as what it is |
| How long does `submit` wait when a lane is full? | up to the job's deadline (default 1 s), then `SubmitError::Full`; `try_submit` never waits | bounded, no reachable hang |
| Typed payloads? | bytes in Tier 1; an optional `Codec` trait in Tier 2 | zero-copy for bytes; no serde dependency |
| Is a panic temporary or permanent by default? | temporary, with the retry budget as the brake | most panics in handlers come from bad external state |
| Async handlers before the executor crate exists? | the `Executor` trait only; no built-in executor | single purpose |

---

## 7. Failure model

What can go wrong, and what task-io guarantees when it does.

| Failure | Guarantee | Test |
|---|---|---|
| a handler panics | caught; recorded as a failure with the payload; retried or dead-lettered; the worker keeps running | unit, W-09 |
| a handler hangs | its lease expires; work rescue moves its lane to a spare worker; the job is retried; the late result is rejected by the fencing token | W-10, lease suite |
| a downstream service is down | temporary failures back off with jitter; the retry budget stops a storm; jobs end in the DLQ with every reason when retries run out | R-01, R-02 |
| submit loop / timer flood / one tenant floods | limits and quotas refuse or delay new work before memory is committed | W-07, W-08 |
| the same job is submitted twice with a key | the second is refused inside the window | K-01 |
| out of memory in an allocation the pool makes | pool structures are pre-reserved; a failed allocation returns a typed error; no partial job is stored | fault-injecting allocator test |
| process crash (kill -9, power loss) | memory store: pending jobs are lost (documented). Durable store: every accepted job survives; leased jobs are retried after their lease expires; no idempotency key commits twice | P-02, crash-at-every-step suite |
| a corrupt record in a durable store | never runs; goes to the DLQ with the reason | corruption suite |
| a node is lost or partitioned (0.6.0) | its leases expire; jobs are re-placed; fencing tokens reject late results | Jepsen-style suite |
| shutdown with work in flight | `shutdown(deadline)` stops accepting jobs, lets running handlers finish until the deadline, returns unstarted jobs to the store, then stops; when it returns, every worker thread is joined | shutdown suite |
| `panic = "abort"` in the user's profile | a handler panic ends the process; with a durable store, the job is retried after restart; the docs say so | one test with abort |

---

## 8. Security responsibilities

- **Hostile input is the default.** Payloads, cron expressions, durable records, and cluster messages are untrusted: size limits before allocation, fuzzed parsers and decoders, typed errors.
- **No code over the wire.** Remote nodes run only registered, named, versioned job types.
- **Authenticated peers only.** Cluster transport requires mutual authentication (`tls-net`); membership changes are raft-committed.
- **Integrity and confidentiality at rest.** Durable records carry a checksum; a corrupt record never runs. Encryption at rest through `crypt-io` for stores that hold sensitive payloads (emails, tokens).
- **Isolation between tenants.** Per-tenant quotas and weighted lanes so one tenant cannot starve another.
- **No secrets in events by default.** Events carry ids, names, and reasons, not payloads, unless a sink opts in.
- **Every security measure states its cost**, in latency and memory, measured; every performance shortcut states its security effect.

---

## 9. Ecosystem and dependencies

### 9.1 task-io's own dependencies

None at runtime. Dev-only tooling is allowed: `criterion`, `proptest`, `loom` (only under `cfg(loom)`). Competitor crates for comparison live in `compare/`, a separate unpublished crate, never in task-io's dependency graph (D-010).

### 9.2 First-party crates task-io works with

| Crate | Relationship | When |
|---|---|---|
| `store-io` | backs the durable store (O-5) | 0.4.0 |
| `crypt-io` | encryption at rest for durable stores | 0.4.0 |
| `log-io`, `metrics-lib` | implement `EventSink` | any |
| `smtp-net` | uses task-io for mail jobs; classifies failures | 0.5.0 |
| `service-kernel` | runs recurring service work on task-io | 0.5.0 |
| the executor crate (O-1) | can run async handlers through the `Executor` seam; task-io never depends on it | when it exists |
| `spawn-io` | uses task-io's store and events for its mission ledger | after 0.4.0 |
| `raft-io`, `rpc-io`, `tls-net` | cluster membership, transport, authentication | 0.6.0 |

Any first-party dependency must be a published version, never a `path` dependency to a sibling repository.

### 9.3 Names

`task-forge` was planned as the in-process scheduler (`LIBRARIES.md` 3.2). It was never built, and the name is taken on crates.io by another author (`task_forge`; crates.io treats `-` and `_` as the same). That role is now the executor crate (O-1).

---

## 10. Phases

Summary only; exit criteria live in [`ROADMAP.md`](./ROADMAP.md).

| Version | Name | Delivers |
|---|---|---|
| 0.1.0 | Scaffold | repository, template, this plan, the performance contract, the R&D program |
| 0.2.0 | R&D and design lock | competitor harness and baselines, experiments with verdicts, public API locked in `docs/API.md` |
| 0.3.0 | Local pipeline | workers, lanes, submit, back-pressure, delays, cron, retries, DLQ, idempotency, leases, work rescue, memory store, events, simulation |
| 0.4.0 | Durable | `Store` final, durable store, group commit, checksums, encryption, crash-at-every-step |
| 0.5.0 | Integration | web stack jobs, smtp-net, service-kernel, the `Executor` seam |
| 0.6.0 | Cluster | dispatch across nodes, fenced leases by raft term, partition testing |
| 0.7.0 | Advance | second R&D round; adopt what wins; all gates re-run |
| 0.9.0 | Hardening | fuzzing, 72-hour soak, chaos, adversarial review, audit |
| 1.0.0 | Stable | frozen after three consumers have run it in production |

---

## 11. Testing and verification

- **Unit and doc tests** for every public item, both `Ok` and every `Err` path. Naming: `test_<subject>_<condition>_<expected>`.
- **`loom` models** for every lock-free structure and every job state transition in the memory store.
- **Miri** for every module with `unsafe`.
- **Sanitizers** on Linux nightly: ThreadSanitizer and AddressSanitizer on the stress suite.
- **Property tests** (`proptest`): lanes against a model, DRR shares against the weights, the timer wheel against a sorted-list model, cron next-fire against a brute-force model, the job state machine against a model, the dedupe window against a model.
- **Deterministic simulation:** thousands of seeds per CI run over workers, lanes, retries, leases, and store faults; any failing seed becomes a permanent regression test.
- **Stress tests** on Linux, macOS, and Windows: submit storms, panic storms, stuck handlers, shutdown under load, with leak checks (threads and memory return to baseline).
- **Fault injection:** a handler that panics at every point, a failing allocator, a store that fails or crashes at every step, a clock that jumps forward and backward.
- **Crash tests:** kill the process at every step of the job state machine for every durable store; 0 lost jobs, 0 duplicate effects per key.
- **Fuzzing:** the cron parser, the payload codec, durable record decoding, cluster messages.
- **Benchmarks:** the full contract in [`BENCHMARKS.md`](./BENCHMARKS.md), gated.
- **Edge cases tested, not assumed:** zero workers requested, one worker, more workers than cores, a limit of zero, a limit of `usize::MAX`, an empty payload, a payload of exactly the limit and one byte over, a handler that is not registered, a cron expression that never fires, a delay of zero and of `Duration::MAX`, a clock moved backward, shutdown during submit, submit during shutdown, a handler that submits jobs, a handler that calls `shutdown`, and the same key submitted from 32 threads at once.

---

## 12. Open decisions

These are James's calls. Each is written so it can be answered with one line.

| Id | Question | Recommendation |
|---|---|---|
| O-1 | The general-purpose executor (async tasks, work stealing, fork-join) that was moved out of task-io: name it, and when to start it? The design is saved in `_strategy/EXECUTOR-SEED/`. | `cycle-exec`, as `CYCLE_STRATEGY.md` already plans; start when Hive DB's executor or LexerSketch needs it |
| O-2 | Publish `0.1.0` to crates.io now to claim the `task-io` name? | yes; the name is free today and the scaffold builds clean |
| O-3 | REPS says "MUST use tokio". task-io uses its own threads and no runtime. Record task-io as an exception, or change the REPS rule portfolio-wide to "no rented runtime" (same question as net-io O-1)? | change REPS portfolio-wide |
| O-4 | If, after every experiment, one gated benchmark cannot reach the 1.20 factor, the release stays blocked. Confirm that only you can waive a single benchmark, in writing in section 14. | yes, keep the block |
| O-5 | Durable store: use `store-io` (first-party, published version), or ship a small internal log? | `store-io` once its Submitter ships in a published version |
| O-6 | Reference hardware for the gates is this machine (Windows) and its WSL2 Ubuntu. Add a bare-metal Linux box and a Mac when available? | yes when available; until then Windows and WSL2 gate, macOS is reported only |
| O-7 | Name of the main type: `Pool`? | `Pool` |

---

## 13. Ideas

The idea register, with hypothesis, method, cost, and status for each, lives in [`RESEARCH.md`](./RESEARCH.md) section 4. Adopted ideas move into section 5 with a decision in section 14; dropped ideas stay in RESEARCH with the evidence.

---

## 14. Decision log

| Id | Date | Decision | Reason |
|---|---|---|---|
| D-001 | 2026-10-13 | Template aligned with net-io: edition 2024, MSRV 1.85, `Apache-2.0 OR MIT`, canonical `REPS.md`, `dev/` committed and `.dev/` private, no committed lockfile | portfolio consistency; the May scaffold (edition 2021, MSRV 1.75, Apache only, a stub REPS) was never published |
| D-002 | 2026-10-13 | Scope is the task dispatch pipeline: workers, lanes, delays, cron, retries, DLQ, idempotency, leases, stores, then dispatch across nodes. Local first, distributed second | the original purpose (`LIBRARIES.md`: "distributed task dispatch"); the web stack needs a local job queue and cron first (`WEB-STACK-DELIVERABLES.md` 7); distribution needs a correct local pipeline under it |
| D-003 | 2026-10-13 | No tokio and no third-party runtime dependency; workers are task-io's own threads | ours, not rented; a pipeline cannot beat the competitors by depending on them (REPS exception pending O-3) |
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
