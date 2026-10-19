# task-io competitor harness

The benchmarks that set task-io's targets. It measures every competitor listed in [`../dev/BENCHMARKS.md`](../dev/BENCHMARKS.md) section 3 by the protocol in that file; task-io must beat the best of them by a factor of at least 1.20 on every row.

This crate is never published and is never a dependency of task-io. It has its own `[workspace]` and a committed lockfile, so competitor crates stay out of task-io's dependency graph and their versions stay pinned.

## Running

```bash
cargo run --release -- all --host R1
```

| Option | Meaning |
|---|---|
| `W-01` ... `W-13`, `D-01`, `D-02`, `P-01`, `P-02`, `R-01`, `R-02`, `K-01`, or `all` | suites to run |
| `--host R1` | host id from BENCHMARKS section 2; names the results file |
| `--quick` | smaller workloads for checking the harness; results go to `results/*-quick.md`, which is not committed and never used for gating |
| `--only a,b` | run only these contenders |
| `--workers 1,4,16,32` | worker counts |

On this Windows machine, link with rust-lld as described in `.claude/CLAUDE.md`. On R2 (WSL Ubuntu), run from a Linux target directory, for example `CARGO_TARGET_DIR=$HOME/target/task-io-compare`.

Results are appended to `results/<date>-<host>.md` (committed) and the raw samples to `results/raw/` (not committed).

## Contenders

| Name | What it is |
|---|---|
| `crossbeam-pool` | `std::thread` workers on a `crossbeam-channel` MPMC queue, the pattern from the crossbeam docs |
| `rayon` | `rayon::ThreadPool::spawn` |
| `threadpool` | `threadpool::ThreadPool::execute` |
| `tokio-blocking` | `spawn_blocking` with the blocking pool capped at the worker count |
| `crossbeam-bounded` | the same pool on a bounded channel of 65,536 slots (task-io's default lane capacity); refuses when full |
| `tokio-spawn` | plain tokio tasks on that many worker threads; tokio's fastest path for short jobs, added to raise the bar |
| `tokio-blocking-elastic` | `spawn_blocking` with tokio's default blocking pool (grows to 512 threads, retires idle ones after 10 s); the elastic baseline in W-12 and W-13 |
| `apalis` | apalis with its in-memory backend, `concurrency(workers)`, parallelized onto a tokio runtime. **apalis-core 1.0.0's in-memory backend panics when a push races the worker** (`memory.rs:246`, `try_lock().unwrap()` on a sink both sides lock), so it runs only as a smoke check that records the failure; apalis on SQLite is planned for the durable suites |
| `best-fixed` | crossbeam-pool at the fixed size that did best on W-12, found by a grid search |
| `weighted-crossbeam` | one bounded channel per lane with a weighted pick (W-06; no pool has lanes) |
| `heap-scheduler` | a scheduler thread over a `BinaryHeap` with a condvar, lazy cancellation, due jobs handed to crossbeam-pool (D-01) |
| `tokio-timer` | one tokio task per delayed job awaiting `sleep_until`, cancelled by `abort` (D-01) |

| `apalis-sqlite` | apalis on SQLite (P-01), measured with `journal_mode = WAL` and `synchronous = FULL` on every connection; its own `setup()` default (`synchronous = OFF`, not power-safe) is a labelled reference row |
| `affinitypool` | `affinitypool::Threadpool::new(workers)` with its default shards, as its own benchmarks use it. It has no fire-and-forget (dropping a job's future cancels it), so the adapter keeps every handle, boxed, in per-thread shards and drops finished ones as it goes: the cost of fire-and-forget through a generic API |
| `rusty-pool` | `rusty_pool` as a fixed pool (core = max = workers, threads started up front); jobs wrapped in `catch_unwind`, since a panic costs it a thread restart |
| `rusty-pool-elastic` | `rusty_pool` with its documented defaults (max twice the logical CPUs, 60 s keep-alive), core at the worker count; an elastic row in W-12 and W-13 |
| `executors-ws` | the `executors` crate's `crossbeam_workstealing_pool::small_pool`, the pool its README shows fastest; jobs wrapped in `catch_unwind`. Its `shutdown` drops queued jobs, which W-11 records. Built with `--features ws-no-park` (a separate build), it is named `executors-ws-no-park`: workers never park |
| `kanal-pool` | crossbeam-pool's design on an unbounded `kanal` channel |
| `a3s-lane` | `a3s-lane` built with its core queue only, one lane with the worker count as its concurrency on a tokio runtime of that many threads. Its commands are async, so each closure runs in `block_in_place` (run plainly inline, a blocking job stalled its scheduler and W-04 deadlocked; `spawn_blocking` was slower). A sync caller blocks on its async `submit`; the result receiver is dropped (the job still runs) |
| `tokio-cron` | tokio-cron-scheduler, one `Job::new_async` per schedule (D-02) |
| `apalis-cron` | one apalis worker per schedule on a `CronScheduler` backend (D-02) |
| `proto-ring` | **an experiment, not a competitor**: task-io's prototype dispatcher (RESEARCH I-01, I-04, I-05, I-16, I-27, I-36); its rows say "experiment" and never set the bar |
| `proto-shards` | **an experiment, not a competitor**: proto-ring's pool on 16 per-submitter rings drained in place by the workers (RESEARCH I-35, I-03) |

R-01, R-02, and K-01 also run against `apalis-sqlite` (retry layer with exponential backoff; idempotency keys), and P-02 kills its worker process half way and restarts it (the harness re-launches itself with `--child`; each worker process gets a unique worker name, since a restarted worker that reuses the dead one's name processes nothing until the old registration expires).

`compare/fang/` is a second, smaller harness for fang (P-01, P-02, R-01, R-02, K-01): fang's SQLite backend (sqlx 0.8) links a different `libsqlite3-sys` than apalis-sqlite (sqlx 0.9), and Cargo allows one per build. It shares `report.rs`, `stats.rs`, and `sys.rs` and writes to the same `compare/results/` files. Run it with `cargo run --release -- all --host R1` from `compare/fang`. fang runs at full durability (WAL; sqlx leaves `synchronous` at SQLite's default, FULL), with 32 workers, a 10 ms worker sleep, and its smallest backoff, 1 s (R-02: 1 s, then 2 s).

fang 0.11 can run one job on two workers (it claims with a read and then a separate write), so the fang suites count distinct job ids, never raw completions, and report duplicate runs. They also report how many workers left the pool: a store error after a job (such as losing that race) ends a fang worker, and fang's pool restarts only workers that panicked (RESEARCH I-39, I-40). A run stops waiting after 60 s without progress, since a job fang left `in_progress` is never fetched again.

`compare/effectum/` does the same for effectum (P-01, P-02, R-01, R-02; it has no idempotency keys, so no K-01), for the same reason: it links SQLite through rusqlite 0.31. effectum opens its database with `PRAGMA journal = wal`, which SQLite ignores (the pragma is `journal_mode`), so it runs in rollback-journal mode with `synchronous = NORMAL` on its writer. The gated P-01 rows measure it that way, as a user gets it, and say so; a second P-01 row enables WAL before effectum opens the file (the mode its code intends) and is a labelled reference, since WAL with `synchronous = NORMAL` can lose commits on power loss. The retry suites read effectum's own `jobs` table to count jobs it failed for good.

Still to add: background-jobs (AGPL-3.0; waiting on PLAN O-9); a3s-lane's priority lanes in W-06 (it runs the generic suites now); and the contenders that need a local PostgreSQL or Redis: fang on PostgreSQL, graphile-worker, sqlxmq, apalis-postgres, apalis-redis. BENCHMARKS asks for fang's fastest local backend, and until PostgreSQL is measured the SQLite rows are provisional. P-03 has an absolute target only (task-io).

`compare/models/` holds loom models of the prototypes' concurrent protocols (the wake protocol of `proto-ring` and `proto-shards` so far). Run them on Linux with `cargo test --release -- --test-threads=1` (about 3 minutes); on Windows, loom's deadlock report overflowed the stack. The suite includes the protocol without the lost-wakeup fix, and passes only if loom finds that deadlock.

Building on Windows: the SQLite contenders compile a bundled C library. With no MSVC compiler installed, set `CC=clang-cl` and `AR=llvm-lib` (LLVM), plus `INCLUDE` for the Windows SDK headers.

## How the measurements stay fair

- Every contender takes the closure through its own generic API, so none pays for an extra box.
- Completion is counted on 64 padded stripes, so the harness's own counter does not add contention that would hide differences at 32 workers.
- Each workload warms the pool up before measuring.
- Medians come with a 95% bootstrap confidence interval (fixed seed, so a table can be recomputed from the raw samples).
- Latency is recorded per job in its own slot and turned into a histogram after the run.
- The idle-latency gap is a busy-wait, because OS sleep granularity would make a million samples take hours; the workers still park during it.

## Known limits

- W-07 (tenant isolation) has no competitor, so it is measured on task-io only.
- W-11 ranks only contenders that ran every accepted job before shutdown returned; tokio tasks and rayon drop or abandon queued work there, and are listed but cannot set the bar.
- Context switches (W-05) are only exposed on Linux; on Windows the row says so.
- Resident memory after a warm-up batch mostly reflects allocator reuse; the heap counter is the primary W-04 metric.
