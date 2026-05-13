//! Distributed task scheduling and dispatch pipeline for Rust. Tasks flow across nodes with delivery guarantees, retries, priority lanes, dead-letter handling, and back-pressure. The enterprise companion to in-process schedulers like task-forge.
//!
//! # Status
//!
//! This crate is in early scaffolding. The public API is not yet
//! defined. See [the repository](https://github.com/jamesgober/task-io)
//! and `.dev/ROADMAP.md` for the milestone plan.

#![doc(html_root_url = "https://docs.rs/task-io")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Crate version string, populated by Cargo at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
