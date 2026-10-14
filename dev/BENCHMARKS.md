# task-io - Performance Contract

> The benchmarks task-io must win, how they are measured, and the gate that blocks a release. This file is binding: [`PLAN.md`](./PLAN.md) section 4.1 and decision D-006 point here. Changing a workload, a competitor, a metric, or the gate needs a decision from James in PLAN section 14.

---

## 1. The gate

For every benchmark in section 4:

1. Measure task-io and every listed competitor by the protocol in section 3.
2. Take the **best competitor** on the benchmark's primary metric.
3. Compute the **speedup factor**:
   - throughput metrics: `task_io / best`
   - time, latency, memory, CPU metrics: `best / task_io`
4. **Pass** when the lower bound of the 95% confidence interval of the speedup factor is **at least 1.20**, on **every** reference host (section 2), for **every** gated statistic of the metric (p50 and p99 for latency, the median for throughput and time).
5. Absolute targets (marked **abs**) have no competitor and must be met exactly as written.

The stretch target is a factor of 2.0. It is reported, not gated.

A miss is a release blocker. The response to a miss is, in order: profile, find the cause, run the experiments in [`RESEARCH.md`](./RESEARCH.md) that address it, and add new ones. If every option is exhausted, stop and report to James with the numbers, the profiles, and what was tried. **Never** lower a target, remove a benchmark, weaken a competitor's configuration, shrink a workload, or change the metric to make a number pass.

Once a benchmark passes, its result becomes the baseline. A later change that makes it more than 5% worse is rejected (REPS performance gate), even if it still beats the competitors.

---

## 2. Reference hosts

| Id | Machine | OS | Notes |
|---|---|---|---|
| R1 | AMD Ryzen 9 9950X3D, 16 cores / 32 threads, two CCDs (CCD0 with 3D V-Cache), 128 GB | Windows 11 Pro (26200) | James's development machine |
| R2 | same machine | Ubuntu on WSL2 | Linux kernel paths (futex, sysfs, epoll); WSL2 is a VM, so a bare-metal Linux host replaces or joins it when available (PLAN O-7) |
| R3 | none yet | macOS | correctness in CI; performance reported, not gated, until a reference Mac exists |

Each results file records the exact CPU, OS build, kernel, power plan, `rustc` version, and every crate version.

---

## 3. Measurement protocol

- **Harness.** All comparisons run from `compare/`, a separate crate in this repository with its own `[workspace]` and lockfile, never published (PLAN D-010). task-io's own `benches/` hold criterion benches of internal hot paths for regression tracking.
- **Same build settings for everyone:** `opt-level = 3`, `lto = "fat"`, `codegen-units = 1`, `panic = "unwind"`, the same `rustc`, `--release`, no `target-cpu=native` unless every contender gets it too (reported both ways).
- **Same resources:** the same worker count for every runtime in a row (1, 4, 16, and 32 threads; rows marked), the same machine, no other load, Windows power plan "High performance" or Linux governor `performance`.
- **Competitors at their best.** Each competitor uses the latest stable release at the time of measurement, pinned in `compare/Cargo.toml`, its recommended configuration, and its idiomatic fastest API for the workload, with a comment citing the competitor's documentation. Any doubt about a competitor's configuration resolves in the competitor's favor. A competitor's maintainer reading the harness should agree it is fair.
- **Statistics:** criterion with at least 100 samples or 10 seconds of measurement per point, after warm-up; latency distributions from at least 1,000,000 samples with HdrHistogram-style recording; report median, p99, p99.9, and the 95% confidence interval.
- **Memory:** heap bytes through a counting global allocator in the harness, and resident set size from the OS.
- **Allocations per operation:** the counting allocator, measured after a warm-up of 1,000,000 operations.
- **Reproducible:** `cargo run --release -p compare -- <suite>` reproduces any table. Results are committed as `compare/results/<YYYY-MM-DD>-<host>.md`.
- **Failing contenders:** if a competitor cannot run a workload (missing feature), the row says so and the remaining competitors set the bar. A competitor that only runs on Linux (glommio, monoio) sets the bar on R2.

---

## 4. Benchmarks

Columns: id, workload (exact), primary metric, statistics gated, competitors.

### 4.1 Executor (L1)

| Id | Workload | Primary metric | Gated | Competitors |
|---|---|---|---|---|
| E-01 | spawn 1,000,000 tasks that return immediately, then join all; 1, 4, 16, 32 workers | wall time | median | tokio (multi-thread; current-thread for 1 worker), smol / async-executor, glommio, monoio |
| E-02 | spawn-to-first-poll latency, 1,000,000 samples, workers idle and workers busy | latency | p50, p99 | tokio, async-executor, glommio |
| E-03 | skynet: a tree of 1,000,000 leaf tasks, branching factor 10, each leaf returns its id, parents sum children | wall time | median | tokio, async-executor, glommio, `may` (stackful) |
| E-04 | ping-pong: two tasks pass a token 1,000,000 times through a oneshot or notify; same worker and forced cross-worker | round-trip time | p50, p99 | tokio, async-executor, glommio |
| E-05 | spawn chain: each task spawns the next, depth 1,000,000 | wall time | median | tokio, async-executor |
| E-06 | fairness: 10,000 tasks each yield 1,000 times | throughput; worst gap between two polls of one task | median; max gap (lower is better) | tokio, async-executor |
| E-07 | fork-join CPU: `fib(40)` by recursive join (no cutoff and cutoff at 20); n-queens 12 by scope; sum of 1,000,000,000 `u64` by recursive split | wall time | median | rayon (`join`, `scope`), tokio (spawn + join), `std::thread::scope` |
| E-08 | mixed: 100,000 async tasks with 1 ms think time plus 1,000 blocking calls of 100 microseconds busy work | latency of the async tasks | p99 | tokio (`spawn_blocking`), async-executor + `blocking` |
| E-09 | idle: runtime started, nothing to do, 60 seconds | wakeups per second; CPU time | **abs: 0 wakeups**, CPU time vs tokio | tokio, async-executor |
| E-10 | memory: 1,000,000 tasks parked on a pending future | bytes per task (heap and RSS); heap allocations per spawn in steady state | median; **abs: 0 allocations** for futures up to the inline size class | tokio, async-executor, glommio, `may`, Go goroutines (reference only, not gated) |
| E-11 | runtime start and stop; shutdown with 1,000,000 pending tasks | time | median | tokio, async-executor |
| E-12 | 32 external OS threads each spawn 100,000 tasks into one runtime | wall time | median | tokio, async-executor |

### 4.2 Timers

| Id | Workload | Primary metric | Gated | Competitors |
|---|---|---|---|---|
| T-01 | insert 1,000,000 timers uniformly over 0 to 10 s, cancel 50%, let the rest fire | ns per insert, per cancel; firing lateness | median; lateness p99 | tokio time |
| T-02 | 100,000 tasks in a 1 ms sleep loop for 10 s | CPU time; lateness | median; p99 | tokio time |

### 4.3 Channels and synchronization

| Id | Workload | Primary metric | Gated | Competitors |
|---|---|---|---|---|
| C-01 | bounded MPSC, capacity 1,024, 1 producer, 1 consumer, 10,000,000 messages of 8 bytes | messages per second | median | tokio mpsc, crossbeam-channel, flume, kanal |
| C-02 | same with 32 producers | messages per second | median | same |
| C-03 | oneshot: create, send, receive | ns per operation | median, p99 | tokio oneshot, futures oneshot |
| C-04 | semaphore acquire/release under 32-way contention | ns per pair | median, p99 | tokio Semaphore, async-lock |

### 4.4 Actors (L2)

Workloads follow the Savina actor benchmark suite (Imam and Sarkar, 2014) with the parameters written here.

| Id | Workload | Primary metric | Gated | Competitors |
|---|---|---|---|---|
| A-01 | ping-pong: 2 actors, 1,000,000 round trips | wall time | median | actix, ractor, kameo, xtra |
| A-02 | thread ring: 503 actors, token passed 10,000,000 hops | wall time | median | same |
| A-03 | counting: one actor receives 10,000,000 increments | wall time | median | same |
| A-04 | fork-join throughput: 64 actors, 1,000,000 messages each | wall time | median | same |
| A-05 | fork-join creation: create 1,000,000 actors, each handles one message and stops | wall time | median | same |
| A-06 | big: 1,000 actors send 1,000,000 messages to random peers | wall time | median | same |
| A-07 | chameneos: 100 creatures, 2,000,000 meetings | wall time | median | same |
| A-08 | memory: 1,000,000 idle actors | bytes per actor | median | same |
| A-09 | supervision: an actor panics; time until the restarted actor handles its next message | latency | p50, p99 | ractor, kameo (actix has no equivalent; noted) |
| A-10 | ask (request and reply) round trip | latency | p50, p99 | same as A-01 |

### 4.5 Jobs (L3)

| Id | Workload | Primary metric | Gated | Competitors |
|---|---|---|---|---|
| J-01 | memory store: 1,000,000 immediate jobs, 32 workers | enqueue-to-start latency; throughput | p50, p99; median | apalis (memory backend), fang |
| J-02 | durable store: same, each enqueue durable before acknowledgment | enqueue-to-durable latency; throughput | p50, p99; median | apalis with its fastest durable backend that runs locally, fang |
| J-03 | 1,000,000 delayed jobs spread over 60 s | firing lateness; CPU time | p99; median | same |
| J-04 | kill -9 with 1,000,000 pending jobs, restart | time to resume; lost jobs; duplicate effects per idempotency key | median; **abs: 0 lost, 0 duplicates** | same (where supported) |
| J-05 | 10% of jobs fail and retry with backoff | completed jobs per second | median | same |

### 4.6 Stability under stress

| Id | Workload | Primary metric | Gated | Competitors |
|---|---|---|---|---|
| S-01 | offered load at 2x capacity for 60 s | p99 latency of admitted work; peak memory | p99 within 2x of the unloaded p99 (**abs**); memory bounded (**abs**); p99 vs competitors | tokio, async-executor |
| S-02 | one task blocks its worker for 500 ms every second, without `spawn_blocking` | p99 latency of all other tasks | p99 | tokio, async-executor |
| S-03 | 10% of tasks panic | throughput; workers lost | median; **abs: 0 workers lost** | tokio |
| S-04 | 24-hour soak, mixed workload | memory drift; thread count drift | **abs: zero drift** after warm-up | none |

### 4.7 Cluster (L4)

Workloads are finalized in the `0.7.0` design and added here before that phase starts. Planned: remote spawn latency, remote `tell` and `ask` latency and throughput, job failover time after node loss, and recovery after a partition heals. Planned competitors: ractor's cluster support, kameo remote actors, and published Akka and Orleans numbers for context only.

---

## 5. Results

Filled in by `0.2.0` (competitor baselines, which become the numeric targets) and by every later phase (task-io results). One row per benchmark and host.

| Id | Host | Best competitor | Its result | Target (x1.20) | task-io | Factor | Pass |
|---|---|---|---|---|---|---|---|
| (none yet) | | | | | | | |
