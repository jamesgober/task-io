# task-io - API Reference

> Reference for every public item in `task-io`, with examples.
> **Status: draft for the `0.2.0` design lock, not yet approved.** Nothing below is implemented yet; `VERSION` is the only public item in `0.1.0`. Items marked *(pending I-xx)* wait on the experiment named in [`../dev/RESEARCH.md`](../dev/RESEARCH.md); defaults are proposals until the experiments confirm them. The design lock is approved by James (ROADMAP v0.2.0, step 4).

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>

## Table of contents

- [Tier 1: the defaults](#tier-1-the-defaults)
- [`Pool`](#pool)
- [Handlers, `Job`, and `Fail`](#handlers-job-and-fail)
- [`JobBuilder`](#jobbuilder)
- [`Builder`](#builder)
- [Health and events](#health-and-events)
- [Dead-letter queue](#dead-letter-queue)
- [Errors](#errors)
- [Tier 3: seams](#tier-3-seams)
- [Defaults and their reasons](#defaults-and-their-reasons)
- [What each seam gives spawn-io and adapters](#what-each-seam-gives-spawn-io-and-adapters)
- [`VERSION`](#version)

---

## Tier 1: the defaults

```rust
use std::time::Duration;
use task_io::{Fail, Job, Pool};

fn send_email(job: &Job) -> Result<(), Fail> {
    let msg = decode(job.payload()).map_err(Fail::permanent)?; // bad input never retries
    smtp_send(&msg).map_err(Fail::retry)?;                       // network errors retry
    Ok(())
}

fn main() -> Result<(), task_io::Error> {
    let pool = Pool::new()?;
    pool.register("send_email", send_email)?;
    pool.submit("send_email", b"...")?;
    pool.shutdown(Duration::from_secs(10))?;
    Ok(())
}
```

Four calls: create, register, submit, shut down. Every limit has a safe default, the controller sizes and repairs the pool, and the store is in memory.

---

## `Pool`

```rust
pub struct Pool { /* private */ }

impl Pool {
    pub fn new() -> Result<Pool, Error>;
    pub fn builder() -> Builder;

    pub fn register<H>(&self, name: &str, handler: H) -> Result<(), RegisterError>
    where
        H: Fn(&Job) -> Result<(), Fail> + Send + Sync + 'static;
    pub fn register_isolated<H>(&self, name: &str, handler: H) -> Result<(), RegisterError>
    where
        H: Fn(&Job) -> Result<(), Fail> + Send + Sync + 'static;

    pub fn submit(&self, name: &str, payload: &[u8]) -> Result<JobId, SubmitError>;
    pub fn try_submit(&self, name: &str, payload: &[u8]) -> Result<JobId, SubmitError>;
    pub fn submit_after(&self, name: &str, delay: Duration, payload: &[u8]) -> Result<JobId, SubmitError>;
    pub fn submit_at(&self, name: &str, at: SystemTime, payload: &[u8]) -> Result<JobId, SubmitError>;
    pub fn job<'a>(&'a self, name: &'a str, payload: &'a [u8]) -> JobBuilder<'a>;

    pub fn cron(&self, expr: &str, name: &str, payload: &[u8]) -> Result<ScheduleId, CronError>;
    pub fn unschedule(&self, id: ScheduleId) -> Result<(), CronError>;
    pub fn cancel(&self, id: JobId) -> Result<Cancelled, CancelError>;

    pub fn health(&self) -> Health;
    pub fn dlq(&self) -> Dlq<'_>;

    pub fn shutdown(self, deadline: Duration) -> Result<ShutdownReport, ShutdownError>;
}
```

| Method | Behavior |
|---|---|
| `new` | A pool with every default (section [Defaults](#defaults-and-their-reasons)). Fails only if the OS refuses threads or memory. |
| `register` | Binds a name to a plain function. Names are 1 to 64 bytes of `[a-zA-Z0-9_.-]`. Registering an existing name is an error, never a silent replace. |
| `register_isolated` | The same, but jobs run in child worker processes (PLAN 5.5). The application must call [`task_io::worker_main()`](#worker_main) at the start of `main`. *(pending I-25)* |
| `submit` | Accepts the job, or waits for room until the job's deadline (default 1 s), then returns `SubmitError::Full`. Never waits forever, never grows memory past the lane's limit. Returns the new `JobId`. |
| `try_submit` | As `submit`, but never waits: `SubmitError::Full` at once. |
| `submit_after` / `submit_at` | Delayed jobs. Never run early. `submit_at` takes wall-clock time; a time in the past runs now. |
| `job` | The Tier 2 builder for one job (lane, key, deadline, tenant, attempts). |
| `cron` | Runs `name` on a schedule. Five fields (minute, hour, day of month, month, day of week) or six with seconds first. Evaluated in UTC. A day-of-month and day-of-week both set means either matches (Vixie rule). An expression that can never fire is refused. |
| `cancel` | Cancels a job that has not started. A job that already started is not interrupted; the result says which (`Cancelled::NotStarted`, `Cancelled::AlreadyRunning`, `Cancelled::AlreadyDone`). |
| `health` | A snapshot of the pool, lanes, workers, and controller. Never blocks the job path. |
| `shutdown` | Stops accepting jobs, lets running handlers finish until `deadline`, hands every unstarted job back to the store, joins every worker, and reports what happened. A handler still running at the deadline is reported as abandoned (its result will be fenced off). Calling it from inside a handler returns `ShutdownError::FromHandler`. |

```rust
pub struct JobId(/* u64, never reused within a store */);
pub struct ScheduleId(/* u64 */);
pub enum Cancelled { NotStarted, AlreadyRunning, AlreadyDone }
```

### `worker_main`

```rust
pub fn worker_main();
```

Call first thing in `main`. In a normal start it returns at once. When the process was started by task-io as an isolated worker, it serves jobs and exits without returning. *(pending I-25)*

---

## Handlers, `Job`, and `Fail`

```rust
pub struct Job { /* private */ }

impl Job {
    pub fn id(&self) -> JobId;
    pub fn name(&self) -> &str;
    pub fn payload(&self) -> &[u8];
    pub fn attempt(&self) -> u32;             // 1 on the first run
    pub fn key(&self) -> Option<&[u8]>;
    pub fn lane(&self) -> &str;
    pub fn deadline(&self) -> Option<Instant>;
    pub fn extend_lease(&self) -> Result<(), LeaseError>;  // long handlers keep their lease
    pub fn submit(&self, name: &str, payload: &[u8]) -> Result<JobId, SubmitError>; // follow-up jobs
}

pub struct Fail { /* private */ }

impl Fail {
    pub fn retry(reason: impl std::fmt::Display) -> Fail;
    pub fn retry_after(reason: impl std::fmt::Display, delay: Duration) -> Fail;
    pub fn permanent(reason: impl std::fmt::Display) -> Fail;
}
```

- A handler is any `Fn(&Job) -> Result<(), Fail> + Send + Sync + 'static`, a plain function or a closure.
- `Fail::retry` is temporary: the job is retried with exponential backoff and full jitter, inside the handler's retry budget. `Fail::permanent` sends it to the DLQ at once.
- A panic is caught and treated as `Fail::retry` with the panic message as the reason (configurable per handler in Tier 2).
- Reasons are stored up to 1 KiB; longer ones are cut and marked as cut.
- `extend_lease` is for handlers that legitimately run longer than the lease; a handler that stops calling it and runs past the lease is treated as stuck.

---

## `JobBuilder`

```rust
impl<'a> JobBuilder<'a> {
    pub fn lane(self, lane: &str) -> Self;
    pub fn tenant(self, tenant: &str) -> Self;
    pub fn key(self, key: impl AsRef<[u8]>) -> Self;        // idempotency key, up to 128 bytes
    pub fn deadline(self, d: Duration) -> Self;             // also bounds how long submit waits
    pub fn after(self, d: Duration) -> Self;
    pub fn at(self, t: SystemTime) -> Self;
    pub fn max_attempts(self, n: u32) -> Self;
    pub fn submit(self) -> Result<JobId, SubmitError>;
    pub fn try_submit(self) -> Result<JobId, SubmitError>;
}
```

```rust
pool.job("charge_card", &payload)
    .lane("critical")
    .key(order_id)
    .deadline(Duration::from_secs(5))
    .max_attempts(3)
    .submit()?;
```

A job with a key is refused (`SubmitError::Duplicate`) if the same key completed or is in flight within the dedupe window.

---

## `Builder`

```rust
impl Builder {
    pub fn workers(self, range: RangeInclusive<usize>) -> Self;     // floor..=cap; n..=n fixes the size
    pub fn scale_down_when_idle(self, idle_share: f64, for_at_least: Duration) -> Self;
    pub fn lane(self, name: &str, weight: u32) -> Self;
    pub fn lane_capacity(self, jobs: usize) -> Self;
    pub fn tenant_quota(self, tenant: &str, max_queued: usize) -> Self;
    pub fn payload_limit(self, bytes: usize) -> Self;
    pub fn retries(self, policy: RetryPolicy) -> Self;
    pub fn dedupe_window(self, d: Duration) -> Self;
    pub fn lease(self, d: Duration) -> Self;
    pub fn durable(self, dir: impl AsRef<Path>) -> Self;            // task-io's own durable log store
    pub fn store(self, store: impl Store) -> Self;                  // or any other store
    pub fn sealer(self, sealer: impl Sealer) -> Self;               // encryption at rest
    pub fn events(self, sink: impl EventSink) -> Self;
    pub fn executor(self, exec: impl Executor) -> Self;             // async handlers
    pub fn build(self) -> Result<Pool, BuildError>;
}

pub struct RetryPolicy { /* max attempts, base and max backoff, budget share */ }
```

`build` checks every value and returns a `BuildError` naming the bad one (a floor above the cap, a weight of zero, a lane capacity of zero, an unwritable durable directory). Nothing is checked later on the job path.

---

## Health and events

```rust
pub struct Health {
    pub workers: WorkerCounts,           // live, retiring, stuck (zombies), floor, cap
    pub lanes: Vec<LaneHealth>,          // name, depth, oldest ready age, share of the last minute
    pub delayed: usize,
    pub dlq: usize,
    pub controller: ControllerState,     // parked, sampling, scaling up, scaling down, repairing
    pub recent_actions: Vec<Action>,     // the last 32 controller actions with their reasons
}

pub trait EventSink: Send + Sync + 'static {
    fn event(&self, e: &Event<'_>);
}

// Fields borrow from the pool, so an event costs no allocation; a sink that
// keeps one copies what it needs.
#[non_exhaustive]
pub enum Event<'a> {
    Submitted { id: JobId, handler: &'a str, lane: &'a str },
    Started { id: JobId, attempt: u32, worker: WorkerId },
    Completed { id: JobId, took: Duration },
    Retrying { id: JobId, attempt: u32, after: Duration, reason: &'a str },
    DeadLettered { id: JobId, reason: &'a str },
    Stale { id: JobId, worker: WorkerId },                 // a result fenced off (lease moved)
    StoreError { during: &'static str, error: &'a StoreError },
    WorkerAdded { worker: WorkerId, reason: ScaleReason },
    WorkerRetired { worker: WorkerId, reason: ScaleReason },
    WorkerReplaced { worker: WorkerId, replacement: WorkerId, reason: RepairReason },
    BreakerOpened { handler: &'a str, failures: u32 },
    BreakerClosed { handler: &'a str },
}
```

Events never carry payloads. A sink must not block; task-io calls it from the thread where the event happens. With no sink, an event costs one predictable branch. A sink that panics is caught; it is then skipped for the rest of the pool's life and `Health` says so, so a broken sink cannot take workers down.

---

## Dead-letter queue

```rust
impl Dlq<'_> {
    pub fn len(&self) -> usize;
    pub fn list(&self, from: Option<JobId>, limit: usize) -> Vec<DeadJob>;
    pub fn replay(&self, id: JobId) -> Result<JobId, ReplayError>;   // a new attempt record
    pub fn discard(&self, id: JobId) -> Result<(), ReplayError>;
}

pub struct DeadJob {
    pub id: JobId,
    pub name: String,
    pub attempts: Vec<Attempt>,        // each with its start time, duration, and reason
    pub final_reason: String,
}
```

---

## Errors

Every error type is `#[non_exhaustive]`, implements `std::error::Error`, and names what to do.

| Type | Variants (draft) |
|---|---|
| `Error` | `Build(BuildError)`, `Register(RegisterError)`, `Submit(SubmitError)`, `Shutdown(ShutdownError)`, `Os(std::io::Error)` |
| `SubmitError` | `Full { lane }` (wait and retry, or raise `lane_capacity`), `TooLarge { size, limit }`, `UnknownHandler { name }`, `Duplicate { key }`, `Quota { tenant }`, `ShuttingDown`, `Store(StoreError)` |
| `RegisterError` | `AlreadyRegistered { name }`, `InvalidName { name }`, `IsolationUnavailable` |
| `CronError` | `Parse { at, reason }`, `NeverFires`, `UnknownSchedule` |
| `CancelError` | `UnknownJob`, `Store(StoreError)` |
| `LeaseError` | `Expired` (the job was handed to another worker; stop and return) |
| `ReplayError` | `NotInDlq`, `Store(StoreError)` |
| `BuildError` | `Workers { floor, cap }`, `LaneWeight { lane }`, `LaneCapacity`, `PayloadLimit`, `Durable(std::io::Error)` |
| `ShutdownError` | `FromHandler`, `Store(StoreError)` |
| `StoreError` | `Io(std::io::Error)`, `Corrupt { at }`, `Full`, `Sealer(String)` |

---

## Tier 3: seams

Traits for crate authors. task-io depends on none of their implementations (PLAN D-017).

```rust
// Draft for the design lock: the contract and its conformance suite are in
// ARCHITECTURE.md sections 8.1 and 8.3.
pub trait Store: Send + Sync + 'static {
    /// Applies a batch of transitions in order and returns once every one is
    /// applied (memory) or on stable storage (durable). One outcome per
    /// transition; an `Err` means the store itself failed and none of the
    /// batch can be assumed applied.
    fn apply(&self, batch: &[Transition], outcomes: &mut Vec<Outcome>) -> Result<(), StoreError>;
    /// Moves up to `max` due, pending jobs of `lane` to leased, each with a
    /// fresh token, and counts the attempt (one step per job).
    fn lease(&self, lane: LaneId, now_ms: u64, max: usize, out: &mut Vec<Leased>) -> Result<(), StoreError>;
    /// After a restart: every non-terminal job, leased ones back to pending.
    fn recover(&self, each: &mut dyn FnMut(JobRecord)) -> Result<RecoveryReport, StoreError>;
    /// The limits this store enforces (jobs, record size, dedupe window).
    fn limits(&self) -> StoreLimits;
}

#[non_exhaustive]
pub enum Transition {
    Put(JobRecord),                                            // pending, or delayed until its time
    Complete { id: JobId, token: Token },
    Retry { id: JobId, token: Token, at_ms: u64, reason: Reason },
    DeadLetter { id: JobId, token: Token, reason: Reason },
    Expire { id: JobId, token: Token },                        // controller repair: back to pending
    Cancel { id: JobId },
}

#[non_exhaustive]
pub enum Outcome { Applied, Stale, Duplicate { existing: JobId }, NotFound, Full, Illegal }
pub trait Sealer: Send + Sync + 'static {
    /// Encrypts and authenticates `plain`, bound to `aad` (the record's job
    /// id and type, so a sealed payload cannot be moved to another record),
    /// appending to `out`. The sealed bytes carry the key id, for rotation.
    fn seal(&self, aad: &[u8], plain: &[u8], out: &mut Vec<u8>) -> Result<(), SealError>;
    /// Verifies and decrypts; any change to `sealed` or `aad` is an error.
    fn open(&self, aad: &[u8], sealed: &[u8], out: &mut Vec<u8>) -> Result<(), SealError>;
    /// Bytes `seal` adds at most, so stores size-check before allocating.
    fn overhead(&self) -> usize;
}

#[non_exhaustive]
pub enum SealError { Tampered, UnknownKey, TooLarge, Other(Box<dyn std::error::Error + Send + Sync>) }
pub trait Executor: Send + Sync + 'static {
    fn spawn(&self, job: Pin<Box<dyn Future<Output = Result<(), Fail>> + Send>>);
}
pub trait Transport: Send + Sync + 'static { /* 0.6.0 */ }
pub trait Membership: Send + Sync + 'static { /* 0.6.0 */ }
```

Each trait ships with a conformance suite (`task_io::store::conformance`, and the same for the others) that an implementation must pass. The `Store` methods are specified with the durable log format in the design lock (ROADMAP step 4).

---

## Defaults and their reasons

Proposed; each is confirmed or changed by measurement in `0.2.0`.

| Setting | Default | Reason |
|---|---|---|
| workers at start | physical cores | one per core avoids SMT contention for CPU-bound handlers (I-30) |
| floor / cap | 1 / 8 x physical cores (CPU-bound pools capped at logical cores) | blocked handlers need more threads than cores; CPU-bound ones never do (I-23, I-37) |
| scale down | 50% idle for 30 s | threads are cheap to start, so retiring is safe; 30 s avoids churn on bursty loads |
| lanes | one lane, `"default"`, weight 1 | the single-lane fast path has no scheduling cost (I-08) |
| lane capacity | 65,536 jobs | bounds memory; admission by expected wait bounds latency (I-29) |
| submit wait | the job's deadline, default 1 s | bounded, never a silent hang |
| payload limit | 64 KiB | one job cannot hold large memory |
| inline payload | 64 bytes | allocation-free dispatch for typical payloads (I-16, I-36) |
| retries | 10 attempts, backoff 100 ms doubling to 1 h, full jitter | survives an outage of several hours without a storm |
| retry budget | 10% of a handler's recent attempts | caps retry traffic against a failing downstream (I-22) |
| dedupe window | 24 h, bounded entries | covers a day of client retries |
| lease | 30 s, extendable | long enough for most handlers, short enough to repair a stuck one quickly |
| DLQ | 1,048,576 entries | bounded; oldest go to the event sink first |
| controller sampling | 10 ms while jobs run, parked when idle | fast reaction, zero idle wakeups (W-05) *(pending I-21)* |

---

## What each seam gives spawn-io and adapters

| Need | API item |
|---|---|
| a durable mission ledger | `Store` and `Builder::durable` |
| retries and dead letters for missions | `Fail`, `RetryPolicy`, `Dlq` |
| watching missions from outside | `EventSink`, `Health` |
| encryption at rest | `Sealer` |
| async handlers | `Executor` |
| distribution | `Transport`, `Membership` (0.6.0) |

---

## `VERSION`

```rust
pub const VERSION: &str;
```

The crate version, populated by Cargo at build time. This is the only item implemented in `0.1.0`.

```rust
println!("task-io {}", task_io::VERSION);
```
