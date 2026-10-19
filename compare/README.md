# task-io competitor harness

The benchmarks that set task-io's targets. It measures every competitor listed in [`../dev/BENCHMARKS.md`](../dev/BENCHMARKS.md) section 3 by the protocol in that file; task-io must beat the best of them by a factor of at least 1.20 on every row.

This crate is never published and is never a dependency of task-io. It has its own `[workspace]` and a committed lockfile, so competitor crates stay out of task-io's dependency graph and their versions stay pinned.

## Running

```bash
cargo run --release -- all --host R1
```

| Option | Meaning |
|---|---|
| `W-01` ... `W-13`, `D-01`, `D-02`, `P-01`, `R-01`, `R-02`, `K-01`, or `all` | suites to run |
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
| `tokio-cron` | tokio-cron-scheduler, one `Job::new_async` per schedule (D-02) |
| `apalis-cron` | one apalis worker per schedule on a `CronScheduler` backend (D-02) |
| `proto-ring` | **an experiment, not a competitor**: task-io's prototype dispatcher (RESEARCH I-01, I-04, I-05, I-16, I-27, I-36); its rows say "experiment" and never set the bar |

R-01, R-02, and K-01 also run against `apalis-sqlite` (retry layer with exponential backoff; idempotency keys).

Still to add: fang (its SQLite backend links a different `libsqlite3-sys` than apalis-sqlite, so it gets its own small harness crate), background-jobs (AGPL-3.0; waiting on PLAN O-9), and the P-02 and P-03 crash suites.

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
