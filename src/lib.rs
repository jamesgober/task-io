//! # task_io
//!
//! The task runtime for the `-io` crates: one scheduler that runs async
//! tasks, blocking work, and CPU fork-join work on the same set of worker
//! threads. It owns spawning, work stealing, parking, timers, cancellation,
//! panic isolation, task monitors, durable jobs, and, later, dispatch across
//! nodes. Actors and spawns are separate crates built on top of it.
//!
//! It does not own sockets or an IO reactor. `net-io` owns those and plugs
//! into the scheduler through a driver seam, so a server, a database
//! executor, and a language runtime can all share one pool of workers.
//!
//! ## Status
//!
//! Scaffold. The design is written down in `dev/PLAN.md`, the performance
//! contract in `dev/BENCHMARKS.md`, and the milestone schedule in
//! `dev/ROADMAP.md` (all in the
//! [repository](https://github.com/jamesgober/task-io)); the public API
//! begins with `0.2.0`.

#![doc(html_root_url = "https://docs.rs/task-io")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Crate version string, populated by Cargo at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn test_version_matches_manifest() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
        assert_eq!(VERSION.split('.').count(), 3, "expected MAJOR.MINOR.PATCH");
    }
}
