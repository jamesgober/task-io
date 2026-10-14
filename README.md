<h1 align="center">
    <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br>
    <b>task-io</b>
    <br>
    <sub><sup>TASK DISPATCH PIPELINE</sup></sub>
</h1>

<div align="center">
    <a href="https://crates.io/crates/task-io"><img alt="Crates.io" src="https://img.shields.io/crates/v/task-io"></a>
    <a href="https://crates.io/crates/task-io"><img alt="Downloads" src="https://img.shields.io/crates/d/task-io?color=%230099ff"></a>
    <a href="https://docs.rs/task-io"><img alt="docs.rs" src="https://img.shields.io/docsrs/task-io"></a>
    <a href="https://github.com/jamesgober/task-io/actions"><img alt="CI" src="https://github.com/jamesgober/task-io/actions/workflows/ci.yml/badge.svg"></a>
    <a href="https://github.com/rust-lang/rfcs/blob/master/text/2495-min-rust-version.md"><img alt="MSRV" src="https://img.shields.io/badge/MSRV-1.85%2B-blue"></a>
</div>

<br>

<div align="left">
    <p>
        <strong>task-io</strong> is the task dispatch pipeline. You hand it a job (a registered handler and a payload) and it makes sure the job runs: on a worker, in priority order, when it is due, retried with backoff when it fails for a temporary reason, set aside in a dead-letter queue with every reason when it fails for good, never twice under the same idempotency key, and kept across a crash when the store is durable. Later, jobs dispatch across nodes.
    </p>
    <p>
        Workers are plain threads that pull jobs until there are none left, so a handler is a plain function that can do anything a thread can: files, sockets, blocking calls.
    </p>
    <p>
        It is built to beat every existing job pipeline and worker pool on its own benchmarks by at least 20%, with the harness, the workloads, and the numbers published in the repository. Being slower than another library on any of them blocks a release.
    </p>
    <br>
    <hr>
    <p>
        <strong>MSRV is 1.85+</strong> (Rust 2024 edition). No dependencies: the workers, queues, timer wheel, and the OS calls they need are all in this crate.
    </p>
    <blockquote>
        <strong>Status: scaffold (<code>0.1.0</code>).</strong> No functional code yet. The design is in <a href="./dev/PLAN.md"><code>dev/PLAN.md</code></a>, the performance contract in <a href="./dev/BENCHMARKS.md"><code>dev/BENCHMARKS.md</code></a>, the R&amp;D program in <a href="./dev/RESEARCH.md"><code>dev/RESEARCH.md</code></a>, and the schedule in <a href="./dev/ROADMAP.md"><code>dev/ROADMAP.md</code></a>. <code>0.2.0</code> measures every competitor and locks the public API.
    </blockquote>
</div>

<hr>
<br>

## What it will do

| Area | Planned |
|---|---|
| Workers | one per core by default, placed by CPU topology, parked with zero idle wakeups, optionally elastic |
| Submit | now, after a delay, at a time, or on a cron schedule; waits when full, or fails fast with `try_submit` |
| Lanes | weighted priority lanes and per-tenant quotas, deficit round robin, no starvation |
| Failures | handlers return `Fail::retry` or `Fail::permanent`; panics are caught; backoff with full jitter; retry budgets |
| Dead letters | every attempt's reason kept; inspect and replay |
| Idempotency | keys with a dedupe window: the same key never commits twice inside it |
| Leases | fencing tokens, so a stuck or partitioned worker's late result is rejected |
| Stores | memory by default; a durable store with checksums and optional encryption |
| Events | every job state change to an event sink for logs and metrics |
| Testing | a deterministic simulation mode: replay any run from a seed |
| Cluster | dispatch of registered job types across authenticated nodes (later phase) |

<br>

## Planned API

```rust
use std::time::Duration;
use task_io::{Fail, Job, Pool};

fn send_email(job: &Job) -> Result<(), Fail> {
    let msg = decode(job.payload()).map_err(Fail::permanent)?;
    smtp_send(&msg).map_err(Fail::retry)?;
    Ok(())
}

let pool = Pool::new()?;
pool.register("send_email", send_email)?;
pool.submit("send_email", b"...")?;
pool.cron("0 3 * * *", "sweep_sessions", b"")?;
pool.shutdown(Duration::from_secs(10))?;
```

The final API is locked in `0.2.0` and documented in [`docs/API.md`](./docs/API.md).

<br>

## What is not here

- **An async executor:** task-io's workers are threads. Async handlers can run on an external executor through a trait seam.
- **Actors and spawns:** separate crates (planned). task-io dispatches jobs; it does not define a programming model on top of them.
- **A storage engine:** the durable store is backed by a separate storage crate through the `Store` trait.
- **Consensus and the wire protocol between nodes:** the cluster layer uses `raft-io` and `rpc-io` through traits.

<hr>
<br>

## Contributing

See <a href="./dev/DIRECTIVES.md"><code>dev/DIRECTIVES.md</code></a> for engineering standards and the definition of done, and <a href="./dev/BENCHMARKS.md"><code>dev/BENCHMARKS.md</code></a> for the performance gate. Before a PR: `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-features` must be clean.

<br>

<div id="license">
    <h2>License</h2>
    <p>Licensed under either of</p>
    <ul>
        <li><b>Apache License, Version 2.0</b>, <a href="./LICENSE-APACHE">LICENSE-APACHE</a></li>
        <li><b>MIT License</b>, <a href="./LICENSE-MIT">LICENSE-MIT</a></li>
    </ul>
    <p>at your option.</p>
</div>

<div align="center">
  <h2></h2>
  <sup>COPYRIGHT <small>&copy;</small> 2026 <strong>James Gober.</strong></sup>
</div>
