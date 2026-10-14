# task-io - API Reference

> Reference for every public item in `task-io`, with examples.
> **Status: scaffold (`0.1.0`).** The public API is written here at `0.2.0` (design lock); see [`../dev/ROADMAP.md`](../dev/ROADMAP.md) and the planned shape in [`../dev/PLAN.md`](../dev/PLAN.md#6-api-shape-planned).

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>

## Table of contents

- [`VERSION`](#version)

## `VERSION`

```rust
pub const VERSION: &str;
```

The crate version, populated by Cargo at build time.

```rust
println!("task-io {}", task_io::VERSION);
```
