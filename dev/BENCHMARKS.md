# task-io - Performance Contract

> The benchmarks task-io must win, how they are measured, and the gate that blocks a release. This file is binding: [`PLAN.md`](./PLAN.md) section 4.1 and decision D-006 point here. Changing a workload, a competitor, a metric, or the gate needs a decision from James in PLAN section 14.

---

## 1. The gate

For every benchmark in section 4:

1. Measure task-io and every listed competitor by the protocol in section 3.
2. Take the **best competitor** on the benchmark's primary metric.
3. Compute the **speedup factor**:
   - throughput metrics: `task_io / best`
   - time, latency, memory, CPU, and count-of-waste metrics: `best / task_io`
4. **Pass** when the lower bound of the 95% confidence interval of the speedup factor is **at least 1.20**, on **every** reference host (section 2), for **every** gated statistic of the metric (p50 and p99 for latency, the median for throughput and time).
5. Absolute targets (marked **abs**) have no competitor, or must hold whatever the competitors do, and must be met exactly as written.

The stretch target is a factor of 2.0. It is reported, not gated.

A miss is a release blocker. The response to a miss is, in order: profile, find the cause, run the experiments in [`RESEARCH.md`](./RESEARCH.md) that address it, and add new ones. If every option is exhausted, stop and report to James with the numbers, the profiles, and what was tried. **Never** lower a target, remove a benchmark, weaken a competitor's configuration, shrink a workload, or change the metric to make a number pass.

Once a benchmark passes, its result becomes the baseline. A later change that makes it more than 5% worse is rejected (REPS performance gate), even if it still beats the competitors.

---

## 2. Reference hosts

| Id | Machine | OS | Notes |
|---|---|---|---|
| R1 | AMD Ryzen 9 9950X3D, 16 cores / 32 threads, two CCDs (CCD0 with 3D V-Cache), 128 GB | Windows 11 Pro (26200) | James's development machine |
| R2 | same machine | Ubuntu on WSL2 | Linux kernel paths (futex, sysfs); WSL2 is a VM, so a bare-metal Linux host replaces or joins it when available (PLAN O-6) |
| R3 | none yet | macOS | correctness in CI; performance reported, not gated, until a reference Mac exists |

Each results file records the exact CPU, OS build, kernel, power plan, disk (for durable benchmarks), `rustc` version, and every crate version.

---

## 3. Measurement protocol

- **Harness.** All comparisons run from `compare/`, a separate crate in this repository with its own `[workspace]` and lockfile, never published (PLAN D-010). task-io's own `benches/` hold criterion benches of internal hot paths for regression tracking.
- **Same build settings for everyone:** `opt-level = 3`, `lto = "fat"`, `codegen-units = 1`, `panic = "unwind"`, the same `rustc`, `--release`, no `target-cpu=native` unless every contender gets it too (reported both ways).
- **Same resources:** the same worker count for every contender in a row (1, 4, 16, and 32 workers; rows marked), the same machine, no other load, Windows power plan "High performance" or Linux governor `performance`. Durable benchmarks use the same disk and the same durability (data flushed to stable storage before the submit is acknowledged).
- **Competitors at their best.** Each competitor uses the latest stable release at the time of measurement, pinned in `compare/Cargo.toml`, its recommended configuration, and its idiomatic fastest API for the workload, with a comment citing the competitor's documentation. Hand-rolled baselines (a worker pool on `std::thread` plus `crossbeam-channel`) follow the pattern in the crossbeam documentation and are written as tight as we can write them, because a hand-rolled pool is what a user would compare us against. Any doubt about a competitor's configuration resolves in the competitor's favor.
- **Statistics:** criterion with at least 100 samples or 10 seconds of measurement per point, after warm-up; latency distributions from at least 1,000,000 samples with HdrHistogram-style recording; report median, p99, p99.9, and the 95% confidence interval.
- **Memory:** heap bytes through a counting global allocator in the harness, and resident set size from the OS.
- **Allocations per operation:** the counting allocator, measured after a warm-up of 1,000,000 operations.
- **Reproducible:** `cargo run --release -p compare -- <suite>` reproduces any table. Results are committed as `compare/results/<YYYY-MM-DD>-<host>.md`.
- **Failing contenders:** if a competitor cannot run a workload (missing feature), the row says so and the remaining competitors set the bar. A workload no competitor supports uses its absolute targets only.
- **Slow contenders (time box):** a contender too slow to finish a workload's count within its time box (P-01: 120 s per run) is measured on what it completes in that time, and the row states how many operations were measured. The time box never applies to task-io: it always runs the full count.
- **Equal durability:** a durable contender is measured with its data on stable storage before the acknowledgment, whatever its own default is; its default is also reported, labelled, when it is weaker (for example apalis-sqlite's `setup()` sets `synchronous = OFF`).

**Competitor set** (latest stable at measurement time; extend it in `0.2.0` with any faster job or pool crate found in the survey, never shrink it):

| Short name | What it is |
|---|---|
| crossbeam-pool | hand-rolled pool: `std::thread` workers on a `crossbeam-channel` MPMC queue |
| rayon | `rayon::ThreadPool::spawn` |
| crossbeam-bounded | crossbeam-pool on a bounded channel of 65,536 slots (task-io's default lane capacity); refuses when full |
| tokio-blocking | `tokio::task::spawn_blocking` on the multi-thread runtime with the blocking pool capped at the worker count |
| tokio-blocking-elastic | `spawn_blocking` with tokio's default blocking pool, which grows to 512 threads and retires idle ones after 10 s: the elastic baseline (W-12, W-13) |
| tokio-spawn | plain tokio tasks on the worker threads, tokio's fastest path for short jobs |
| weighted-crossbeam | one bounded crossbeam channel per lane with a weighted pick (W-06 only; no pool has lanes) |
| best-fixed | crossbeam-pool at the fixed size that does best on that workload, found by a grid search after the fact (W-12 only: the bar an elastic pool must beat) |
| threadpool | the `threadpool` crate |
| apalis | apalis with its memory backend; with SQLite (WAL, `synchronous=FULL`) for durable rows; apalis-cron for cron. Finding (2026-10-14): apalis-core 1.0.0's memory backend panics when a push races the worker's `poll_ready` (`memory.rs:246`), so it cannot run under concurrent submission; the harness records that on every run until a release fixes it |
| fang | fang with its fastest backend that runs locally. Measured on SQLite (WAL, `synchronous=FULL`) so far; PostgreSQL is still to be measured, and the faster of the two sets the bar. Finding (2026-10-19): fang 0.11 can run one job on two workers, stores duplicate unique jobs under concurrent submits, and loses a worker for good on any store error after a job (RESEARCH section 6, I-39, I-40); the fang suites report those counts on every run |
| background-jobs | the `background-jobs` crate with its memory storage. It is AGPL-3.0, so it is not added to `compare/` until James decides how to measure it (PLAN O-9) |
| tokio-cron | `tokio-cron-scheduler` (cron rows only) |
| heap-scheduler | hand-rolled delayed-job thread on a `BinaryHeap` and a condvar (delayed rows only) |

Sidekiq, Faktory, BullMQ, and Celery numbers may be quoted from their own published benchmarks for context, never as a gate.

---

## 4. Benchmarks

Columns: id, workload (exact), primary metric, statistics gated, competitors.

### 4.1 Workers and dispatch

| Id | Workload | Primary metric | Gated | Competitors |
|---|---|---|---|---|
| W-01 | 10,000,000 jobs with an empty handler and an 8-byte payload; 1, 4, 16, 32 workers; submitted from 1 thread and from 32 threads | wall time | median | crossbeam-pool, rayon, tokio-blocking, threadpool, apalis, background-jobs |
| W-02 | submit-to-start latency, 1,000,000 samples, pool idle and pool 80% busy | latency | p50, p99 | same |
| W-03 | handlers doing 10 us, 100 us, and 1 ms of busy work | overhead: 100% minus jobs per second as a share of the ideal (workers / work time); a share near 100% cannot improve 1.20x, its overhead can | median | same |
| W-04 | 1,000,000 queued jobs with 64-byte payloads | bytes per queued job; heap allocations per submit in steady state | median; **abs: 0 allocations** for payloads up to the inline size | same |
| W-05 | idle pool for 60 s | wakeups per second; CPU time | **abs: 0 wakeups**; CPU time vs competitors | same |
| W-06 | 3 lanes with weights 4:2:1, saturated for 30 s | share error per lane; p99 latency of the top lane | **abs: share error at most 2%**; p99 | crossbeam-pool with one channel per lane and a weighted pick |
| W-07 | 2 tenants; one floods at 10x capacity | p99 latency of the other tenant | **abs: within 2x of its unloaded p99** | none (no competitor has tenant quotas) |
| W-08 | offered load at 2x capacity for 60 s | p99 latency of admitted jobs; peak memory | **abs: p99 within 2x of unloaded; memory bounded**; p99 vs competitors | crossbeam-pool (bounded and unbounded), rayon, tokio-blocking, apalis |
| W-09 | 10% of handlers panic | completed jobs per second; workers lost | median; **abs: 0 workers lost** | crossbeam-pool with `catch_unwind`, rayon, tokio-blocking, threadpool |
| W-10 | one handler blocks for 500 ms every second, 100 us jobs at 85% load, 4 and 32 workers | p99 latency of all other jobs | p99 | crossbeam-pool, rayon, tokio-blocking, apalis |
| W-11 | shutdown with a 10 s deadline and 1,000,000 queued jobs | time to stop with every accepted job run or handed back to the store; threads left | median; **abs: 0 threads left, 0 jobs lost** (a contender that drops queued jobs at shutdown is reported but cannot set the bar) | crossbeam-pool, rayon, tokio-blocking, apalis |
| W-12 | load step: arrivals jump to 10x for 60 s, then drop back (base rate: 10% of 16 workers); run once with 1 ms blocking handlers (IO-like) and once with 100 us CPU handlers. Settled means every later 1 s window's p99 is within 1.2x (or 1 ms) of the p99 over the last third of the step | p99 latency during the step; time to settle; time to retire the extra workers after the drop; worker-count reversals | p99, median, median; **abs: at most 2 reversals per step; no CPU-bound pool above the logical core count** | tokio-blocking-elastic, best-fixed |
| W-13 | worker faults, injected one at a time at 85% load of 1 ms jobs on 4 workers: a handler that hangs for 15 s, a handler that panics in a loop, a worker slowed on purpose, a worker thread ended outside shutdown | time to detect; time until a replacement worker runs jobs; jobs delayed past their deadline | p99; p99; median; **abs: 0 jobs lost, 0 duplicate commits** | crossbeam-pool, crossbeam-bounded, rayon, threadpool, tokio-blocking, tokio-spawn, tokio-blocking-elastic (none detects a slow worker or replaces a stuck one, except that tokio-blocking-elastic adds threads; noted) |
| W-14 | W-01 at 32 workers with the controller and health records on, against a build with them compiled out | throughput cost of the controller | **abs: at most 1%** | none |

### 4.2 Delays and cron

| Id | Workload | Primary metric | Gated | Competitors |
|---|---|---|---|---|
| D-01 | 1,000,000 jobs delayed uniformly over 0 to 10 s; cancel 50% | ns per delayed submit and per cancel; firing lateness | median; lateness p99; **abs: never early** | heap-scheduler, tokio (`sleep` then `spawn_blocking`), apalis scheduled jobs |
| D-02 | 10,000 cron schedules firing every minute for 10 minutes | firing lateness; CPU time | p99; median | apalis-cron, tokio-cron |

### 4.3 Retries and dead letters

| Id | Workload | Primary metric | Gated | Competitors |
|---|---|---|---|---|
| R-01 | 10% of jobs fail temporarily and retry with backoff | completed jobs per second | median | apalis, fang, background-jobs |
| R-02 | the downstream fails 100% for 10 s, then recovers | attempts made during the outage (waste); time to drain after recovery | median; median; **abs: every job either completes or is in the DLQ with every reason** | same |

### 4.4 Idempotency

| Id | Workload | Primary metric | Gated | Competitors |
|---|---|---|---|---|
| K-01 | 10,000,000 submits from 32 threads, 50% duplicate keys inside the window | duplicate runs; submit cost with a key vs without | **abs: 0 duplicate runs**; ns per submit vs competitors that dedupe | apalis and fang where they support unique jobs |

### 4.5 Durable store

| Id | Workload | Primary metric | Gated | Competitors |
|---|---|---|---|---|
| P-01 | 1,000,000 durable submits, data on stable storage before the acknowledgment | submit-to-durable latency; throughput | p50, p99; median | apalis (SQLite), fang, background-jobs (durable storage), each at the same durability |
| P-02 | kill -9 with 1,000,000 pending jobs, then restart | time to resume; lost jobs; duplicate effects per key | median; **abs: 0 lost, 0 duplicates** | same, where supported |
| P-03 | 10,000 runs with injected stalls and crashes during handlers | stale results committed after a lease moved | **abs: 0** | none |

### 4.6 Soak

| Id | Workload | Primary metric | Gated | Competitors |
|---|---|---|---|---|
| S-01 | 24 hours, mixed workload (W, D, R, K at once) with load cycles | memory, thread, handle, and latency drift after warm-up | **abs: zero drift** | none |
| S-02 | 7 days on R2, mixed workload with daily load cycles from 0.1x to 3x capacity, durable store, injected faults at random | drift; controller repairs not caused by an injected fault or a handler | **abs: zero drift; 0 repairs caused by task-io itself** | none |

### 4.7 Cluster (0.6.0)

Workloads are finalized in the `0.6.0` design and added here before that phase starts. Planned: remote dispatch latency and throughput, failover time after node loss, recovery after a partition heals, and 0 stale commits under the Jepsen-style suite. Planned competitors, run locally at the same durability and with authentication on: RabbitMQ through `lapin` (the stack it replaces, `LIBRARIES.md` section 6), NATS JetStream, and Redis Streams. Temporal and Sidekiq numbers are context only.

### 4.8 Moved out

The executor suites (task spawn, wake, fork-join, timers for async tasks, channels) and the actor suite moved with the executor and actor designs to `_strategy/EXECUTOR-SEED/` (PLAN D-015).

---

## 5. Results

Targets come from the best competitor per gated statistic; task-io's columns are filled by each phase. One row per benchmark, variant, and host.

**Status: provisional.** R1 baselines from the full-protocol run of 2026-10-14 (`compare/results/2026-10-14-R1.md`, 4 h 25 min). That run's host load was not verified (other build processes were later seen on the machine), so every R1 row is re-measured in a verified quiet window before it gates anything. Still to measure: W-04 (re-run with start reservations counted), W-05 context switches (R2), D-01, D-02, K-01, P-01 to P-03, R-01, R-02, and every row on R2. W-07 and S-01/S-02 have absolute targets only; W-06 has only the hand-rolled lane pool, whose 32-worker share error (8.7 points) is far above task-io's absolute 2-point target. W-05: every contender used 0 ms of CPU per second while idle on R1, so the target is the absolute 0.

| Id | Host | Variant | Statistic | Best competitor | Its result | Target (x1.20) | task-io | Factor | Pass |
|---|---|---|---|---|---|---|---|---|---|
| W-01 | R1 | 10000000 empty jobs, 1 submitter(s), 1 workers | wall time | crossbeam-bounded | 603 ms | <= 502.5 ms | | | |
| W-01 | R1 | 10000000 empty jobs, 1 submitter(s), 16 workers | wall time | crossbeam-pool | 1902 ms | <= 1585 ms | | | |
| W-01 | R1 | 10000000 empty jobs, 1 submitter(s), 32 workers | wall time | tokio-spawn | 2892 ms | <= 2410 ms | | | |
| W-01 | R1 | 10000000 empty jobs, 1 submitter(s), 4 workers | wall time | crossbeam-pool | 569 ms | <= 474.2 ms | | | |
| W-01 | R1 | 10000000 empty jobs, 32 submitter(s), 1 workers | wall time | crossbeam-pool | 1046 ms | <= 871.7 ms | | | |
| W-01 | R1 | 10000000 empty jobs, 32 submitter(s), 16 workers | wall time | crossbeam-pool | 1245 ms | <= 1038 ms | | | |
| W-01 | R1 | 10000000 empty jobs, 32 submitter(s), 32 workers | wall time | crossbeam-pool | 1222 ms | <= 1018 ms | | | |
| W-01 | R1 | 10000000 empty jobs, 32 submitter(s), 4 workers | wall time | crossbeam-pool | 1007 ms | <= 839.2 ms | | | |
| W-02 | R1 | 80% busy, 10 us jobs, 1000000 samples, 32 workers | p50 latency | crossbeam-bounded | 600 ns | <= 500 ns | | | |
| W-02 | R1 | 80% busy, 10 us jobs, 1000000 samples, 32 workers | p99 latency | tokio-blocking | 98623 ns | <= 82,190 ns | | | |
| W-02 | R1 | 80% busy, 10 us jobs, 1000000 samples, 4 workers | p50 latency | rayon | 200 ns | <= 166.7 ns | | | |
| W-02 | R1 | 80% busy, 10 us jobs, 1000000 samples, 4 workers | p99 latency | rayon | 8807 ns | <= 7339 ns | | | |
| W-02 | R1 | idle pool, 1000000 samples, 32 workers | p50 latency | crossbeam-bounded | 4703 ns | <= 3919 ns | | | |
| W-02 | R1 | idle pool, 1000000 samples, 32 workers | p99 latency | tokio-blocking | 13607 ns | <= 11,340 ns | | | |
| W-02 | R1 | idle pool, 1000000 samples, 4 workers | p50 latency | crossbeam-bounded | 4903 ns | <= 4086 ns | | | |
| W-02 | R1 | idle pool, 1000000 samples, 4 workers | p99 latency | tokio-blocking | 15207 ns | <= 12,670 ns | | | |
| W-03 | R1 | 10 us busy jobs, 1 workers | overhead (100% - share of ideal) | threadpool | 0.4888 % | <= 0.4073 % | | | |
| W-03 | R1 | 10 us busy jobs, 16 workers | overhead (100% - share of ideal) | crossbeam-pool | 3.159 % | <= 2.633 % | | | |
| W-03 | R1 | 10 us busy jobs, 32 workers | overhead (100% - share of ideal) | rayon | 17.94 % | <= 14.95 % | | | |
| W-03 | R1 | 10 us busy jobs, 4 workers | overhead (100% - share of ideal) | crossbeam-pool | 1.108 % | <= 0.9231 % | | | |
| W-03 | R1 | 100 us busy jobs, 1 workers | overhead (100% - share of ideal) | threadpool | 0.0755 % | <= 0.0629 % | | | |
| W-03 | R1 | 100 us busy jobs, 16 workers | overhead (100% - share of ideal) | crossbeam-pool | 0.3798 % | <= 0.3165 % | | | |
| W-03 | R1 | 100 us busy jobs, 32 workers | overhead (100% - share of ideal) | crossbeam-bounded | 6.199 % | <= 5.166 % | | | |
| W-03 | R1 | 100 us busy jobs, 4 workers | overhead (100% - share of ideal) | rayon | 0.1863 % | <= 0.1552 % | | | |
| W-03 | R1 | 1000 us busy jobs, 1 workers | overhead (100% - share of ideal) | crossbeam-pool | 0.0163 % | <= 0.0136 % | | | |
| W-03 | R1 | 1000 us busy jobs, 16 workers | overhead (100% - share of ideal) | crossbeam-bounded | 0.0848 % | <= 0.0707 % | | | |
| W-03 | R1 | 1000 us busy jobs, 32 workers | overhead (100% - share of ideal) | threadpool | 4.803 % | <= 4.003 % | | | |
| W-03 | R1 | 1000 us busy jobs, 4 workers | overhead (100% - share of ideal) | rayon | 0.0323 % | <= 0.0269 % | | | |
| W-08 | R1 | 2x capacity for 60 s, 100 us jobs, 32 workers | p99 latency | crossbeam-bounded | 328,991,000 ns | <= 274,200,000 ns | | | |
| W-08 | R1 | 2x capacity for 60 s, 100 us jobs, 4 workers | p99 latency | crossbeam-bounded | 1,776,290,000 ns | <= 1,480,000,000 ns | | | |
| W-09 | R1 | 1000000 jobs, 10% panic, 32 workers | completed jobs per second | tokio-spawn | 1,989,760 /s | >= 2,388,000 /s | | | |
| W-09 | R1 | 1000000 jobs, 10% panic, 4 workers | completed jobs per second | crossbeam-bounded | 3,310,400 /s | >= 3,972,000 /s | | | |
| W-10 | R1 | 500 ms stuck job every second, 85% load, 30 s, 32 workers | p99 latency | tokio-blocking | 4,710,400 ns | <= 3,925,000 ns | | | |
| W-10 | R1 | 500 ms stuck job every second, 85% load, 30 s, 4 workers | p99 latency | crossbeam-pool | 58,523,600 ns | <= 48,770,000 ns | | | |
| W-11 | R1 | shutdown with 1000000 queued jobs, 32 workers | time to stop, all jobs run | crossbeam-pool | 172 ms | <= 143.3 ms | | | |
| W-11 | R1 | shutdown with 1000000 queued jobs, 4 workers | time to stop, all jobs run | tokio-blocking | 259 ms | <= 215.8 ms | | | |
| W-12 | R1 | load step 10x, 1 ms blocking handlers | p99 latency during the step | tokio-blocking-elastic | 14.21 us | <= 11.84 us | | | |
| W-12 | R1 | load step 10x, 100 us CPU handlers | p99 latency during the step | best-fixed (crossbeam-pool, 32) | 3.70 us | <= 3.08 us | | | |
| W-13 | R1 | one handler hangs for 15 s; 85% load of 1 ms jobs, 4 workers | p99 latency of other jobs | tokio-blocking-elastic | 17.31 us | <= 14.43 us | | | |
