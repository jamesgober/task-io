# task-io competitor harness

The benchmarks that set task-io's targets. It measures every competitor listed in [`../dev/BENCHMARKS.md`](../dev/BENCHMARKS.md) section 3 by the protocol in that file; task-io must beat the best of them by a factor of at least 1.20 on every row.

This crate is never published and is never a dependency of task-io. It has its own `[workspace]` and a committed lockfile, so competitor crates stay out of task-io's dependency graph and their versions stay pinned.

## Running

```bash
cargo run --release -- all --host R1
```

| Option | Meaning |
|---|---|
| `W-01` ... or `all` | suites to run |
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
| `tokio-spawn` | plain tokio tasks on that many worker threads; tokio's fastest path for short jobs, added to raise the bar |

Job crates (apalis, fang, background-jobs) and the cron and delayed-job contenders are added with the suites that use them.

## How the measurements stay fair

- Every contender takes the closure through its own generic API, so none pays for an extra box.
- Completion is counted on 64 padded stripes, so the harness's own counter does not add contention that would hide differences at 32 workers.
- Each workload warms the pool up before measuring.
- Medians come with a 95% bootstrap confidence interval (fixed seed, so a table can be recomputed from the raw samples).
- Latency is recorded per job in its own slot and turned into a histogram after the run.
- The idle-latency gap is a busy-wait, because OS sleep granularity would make a million samples take hours; the workers still park during it.

## Known limits

- Context switches (W-05) are only exposed on Linux; on Windows the row says so.
- Resident memory after a warm-up batch mostly reflects allocator reuse; the heap counter is the primary W-04 metric.
