<h1 align="center">
    <strong>task-io</strong>
    <br>
    <sup><sub>DISTRIBUTED TASK IO PIPELINE FOR RUST</sub></sup>
</h1>

<p align="center">
    <a href="https://crates.io/crates/task-io"><img alt="crates.io" src="https://img.shields.io/crates/v/task-io.svg"></a>
    <a href="https://crates.io/crates/task-io"><img alt="downloads" src="https://img.shields.io/crates/d/task-io.svg"></a>
    <a href="https://docs.rs/task-io"><img alt="docs.rs" src="https://docs.rs/task-io/badge.svg"></a>
    <a href="https://github.com/jamesgober/task-io/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/jamesgober/task-io/actions/workflows/ci.yml/badge.svg"></a>
</p>

<p align="center">
    Tasks delivered across nodes with retries, priority lanes, dead-letter queues, and back-pressure.
</p>

---

## Status

Status: DEFERRED - placeholder repo to claim the crates.io name. Distributed task dispatch is built once cluster coordination (mod-cluster) is in place. See `.dev/ROADMAP.md`.

This repository is published primarily to reserve the crate name and
to establish the project scaffolding. Implementation work proceeds on
the schedule documented in `.dev/ROADMAP.md`.

## What it does

Distributed task scheduling and dispatch pipeline for Rust. Tasks flow across nodes with delivery guarantees, retries, priority lanes, dead-letter handling, and back-pressure. The enterprise companion to in-process schedulers like task-forge.

## License

Licensed under the Apache License, Version 2.0. See [`LICENSE`](LICENSE)
for the full text.

Copyright (C) 2026 James Gober.
