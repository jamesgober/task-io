//! # task_io
//!
//! The task dispatch pipeline for the `-io` crates. You hand it a job (a
//! registered handler and a payload) and it makes sure the job runs: on a
//! worker, in priority order, when it is due, retried with backoff when it
//! fails for a temporary reason, set aside in a dead-letter queue when it
//! fails for good, never twice under the same idempotency key, and kept
//! across a crash when the store is durable. Later, jobs dispatch across
//! nodes.
//!
//! Workers are plain threads, so a handler is a plain function that can do
//! anything a thread can. A controller watches every worker and the load on
//! the pipeline, scales the pool between a floor and a cap, and repairs or
//! replaces workers that are stuck, slow, or failing.
//!
//! task-io has zero dependencies. It is not an async executor, an actor
//! system, or a spawn framework; those are separate crates.
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
