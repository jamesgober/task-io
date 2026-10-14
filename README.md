<h1 align="center">
    <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br>
    <b>task-io</b>
    <br>
    <sub><sup>TASK RUNTIME</sup></sub>
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
        <strong>task-io</strong> is where work runs. One scheduler runs async tasks, blocking calls, and CPU fork-join work on the same set of worker threads, with timers, cancellation, panic isolation, and monitors built in. On top of that come durable jobs (delayed, recurring, retried, dead-lettered, idempotent) and, later, dispatch of tasks and jobs across nodes.
    </p>
    <p>
        It is built to beat every existing runtime on its own benchmarks by at least 20%, with the harness, the workloads, and the numbers published in the repository. Being slower than another library on any of them blocks a release.
    </p>
    <br>
    <hr>
    <p>
        <strong>MSRV is 1.85+</strong> (Rust 2024 edition). No dependencies: the scheduler, task memory, queues, timers, and the OS calls they need are all in this crate.
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
| Tasks | `spawn` for async tasks, `spawn_blocking` for blocking calls, `join` and `scope` for CPU fork-join, `spawn_local` for thread-local tasks, all on one pool |
| Scheduler | work stealing ordered by CPU topology (cores, CCDs, NUMA nodes), zero-allocation spawn in steady state, tickless idle, bounded fairness |
| Time | timer wheel: `sleep`, `interval`, `timeout`, deadlines; never early |
| Safety | panics caught at the task boundary, every queue and channel bounded, typed errors, shutdown with a deadline |
| Monitors | one exit notice per task with the cause (completed, cancelled, panicked), the base for supervisors built on top |
| Jobs | delay, cron, retry with backoff and jitter, dead letters, idempotency keys, priority lanes, leases, pluggable durable store |
| Cluster | remote spawn of registered task types, distributed jobs, authenticated peers only (later phase) |
| Testing | a deterministic simulation mode: replay any interleaving from a seed, for task-io and for your code |

<br>

## Planned API

```rust
use task_io::Runtime;

let rt = Runtime::new()?;
let answer = rt.block_on(async {
    let a = task_io::spawn(async { 20 });
    let b = task_io::spawn_blocking(|| 22);
    Ok::<_, task_io::Error>(a.await? + b.await?)
})?;
assert_eq!(answer, 42);
```

The final API is locked in `0.2.0` and documented in [`docs/API.md`](./docs/API.md).

<br>

## What is not here

- **Actors and spawns:** separate crates built on task-io (`actor-io`, `spawn-io`, planned). task-io runs the work; they define how work is organized and supervised.
- **Sockets and the IO reactor:** they live in <a href="https://github.com/jamesgober/net-io"><code>net-io</code></a>, which plugs into task-io's driver seam so IO wakes tasks with no extra thread.
- **A storage engine:** durable jobs use a pluggable store; the durable one is backed by a separate storage crate.
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
