//! Every interleaving of the ring's push and pop within loom's preemption
//! bound: no data race on a cell (loom checks each access), and every value a
//! push accepted comes out exactly once.
//!
//! Every thread does a fixed number of operations and never waits for
//! another (a producer whose push is refused records it; a consumer that finds
//! the ring empty moves on), because loom cannot bound a thread that spins
//! until another makes progress. After the threads join, the main thread
//! drains what is left.

use loom::sync::Arc;
use loom::thread;
use task_io_models::ring::Ring;

fn model(bound: usize, f: impl Fn() + Sync + Send + 'static) {
    let mut b = loom::model::Builder::new();
    if b.preemption_bound.is_none() {
        b.preemption_bound = Some(bound);
    }
    b.check(f);
}

/// `producers` threads push `per` distinct values each (once each), and
/// `consumers` threads pop `pops` times each, all at once.
fn exchange(ring: Ring, producers: u64, per: u64, consumers: usize, pops: usize) {
    let ring = Arc::new(ring);
    let mut pushers = Vec::new();
    for p in 0..producers {
        let ring = ring.clone();
        pushers.push(thread::spawn(move || {
            (0..per)
                .map(|k| p * per + k)
                .filter(|&v| ring.push(v).is_ok())
                .collect::<Vec<_>>()
        }));
    }
    let mut poppers = Vec::new();
    for _ in 0..consumers {
        let ring = ring.clone();
        poppers.push(thread::spawn(move || {
            (0..pops).filter_map(|_| ring.pop()).collect::<Vec<_>>()
        }));
    }
    let mut accepted: Vec<u64> = pushers
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    let mut out: Vec<u64> = poppers
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    while let Some(v) = ring.pop() {
        out.push(v);
    }
    accepted.sort_unstable();
    out.sort_unstable();
    assert_eq!(out, accepted, "lost or duplicated");
}

#[test]
fn two_producers_two_consumers() {
    model(3, || exchange(Ring::new(2), 2, 1, 2, 1));
}

#[test]
fn wraps_around_and_refuses_when_full() {
    // Three values offered to two cells while a consumer pops: cells are
    // reused, and a push may be refused.
    model(3, || exchange(Ring::new(2), 1, 3, 1, 2));
}

#[test]
fn two_producers_one_consumer_full_ring() {
    model(2, || exchange(Ring::new(2), 2, 2, 1, 2));
}

const RUN_ONE_CELL: &str = "TASK_IO_MODELS_RUN_ONE_CELL";

/// Runs a one-cell ring; only does work in the child process started by
/// `one_cell_ring_loses_values` (a failed loom run aborts while unwinding).
#[test]
fn one_cell_ring_model() {
    if std::env::var_os(RUN_ONE_CELL).is_some() {
        model(2, || exchange(Ring::new_unclamped(1), 1, 2, 1, 1));
    }
}

/// Why `Ring::new` never makes fewer than 2 cells: with 1, a push can
/// overwrite a value that was never popped. loom then reports either the write
/// racing the consumer's read of the cell, or the consumer spinning without
/// end on a cell whose sequence ran past its position (a livelock); which one
/// depends on the interleaving it reaches first.
#[test]
fn one_cell_ring_loses_values() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "one_cell_ring_model", "--test-threads=1"])
        .env(RUN_ONE_CELL, "1")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "a one-cell ring passed");
    assert!(
        text.contains("Causality violation")
            || text.contains("maximum number of branches")
            || text.contains("lost or duplicated"),
        "unexpected failure:\n{text}"
    );
}
