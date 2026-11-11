//! The wake protocol of `compare/src/proto_intake.rs` (experiment I-43),
//! reduced for loom: one thread is the timer, each submitter's ring is
//! one cell (published with a release store, checked with a SeqCst load, as
//! the ring's sequence is), and every drained timer is taken as fired, so
//! the timer then sleeps with no deadline and only a submit can wake it.
//!
//! The property is liveness: the timer drains every submit, so no
//! interleaving ends with a submit waiting and the timer parked (loom reports
//! that as a deadlock). The case it is for: a submit reads a target lowered
//! by an earlier submit, skips the wake because its deadline is not earlier,
//! and the timer, having drained the earlier one, goes to sleep with no
//! deadline. The timer's re-check after it publishes its target catches it.

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering, fence};
use loom::thread::{self, Thread};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IntakeVariant {
    /// As in `proto_intake.rs`.
    Fixed,
    /// The timer parks right after publishing its target, without checking
    /// the rings again.
    NoRecheck,
}

struct Shared {
    cells: Vec<AtomicBool>,
    sleeping_until: AtomicU64,
    /// Set before any submitter starts (the spawn orders it).
    timer: std::sync::OnceLock<Thread>,
}

impl Shared {
    fn submit(&self, me: usize, deadline: u64) {
        self.cells[me].store(true, Ordering::Release);
        fence(Ordering::SeqCst);
        let mut s = self.sleeping_until.load(Ordering::Relaxed);
        while deadline < s {
            match self.sleeping_until.compare_exchange(
                s,
                deadline,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.timer
                        .get()
                        .expect("set before submitters start")
                        .unpark();
                    return;
                }
                Err(v) => s = v,
            }
        }
    }
}

/// One execution: one submitter per entry of `deadlines`, each submitting
/// once, and a timer thread. (With the model's main thread as the timer,
/// loom 0.7.2 failed an internal assertion in `park`.)
pub fn run_intake(variant: IntakeVariant, deadlines: &[u64]) {
    let shared = Arc::new(Shared {
        cells: deadlines.iter().map(|_| AtomicBool::new(false)).collect(),
        sleeping_until: AtomicU64::new(u64::MAX),
        timer: std::sync::OnceLock::new(),
    });
    let total = deadlines.len();
    let timer = {
        let shared = shared.clone();
        thread::spawn(move || {
            let mut drained = 0;
            loop {
                // The ring's single consumer: an acquire load of the cell,
                // then a release store handing it back (no read-modify-write,
                // as in `proto_intake.rs`).
                for c in &shared.cells {
                    if c.load(Ordering::Acquire) {
                        c.store(false, Ordering::Release);
                        drained += 1;
                    }
                }
                if drained == total {
                    return;
                }
                // Nothing left in the wheel: sleep with no deadline.
                shared.sleeping_until.store(u64::MAX, Ordering::SeqCst);
                fence(Ordering::SeqCst);
                if variant == IntakeVariant::Fixed
                    && shared.cells.iter().any(|c| c.load(Ordering::SeqCst))
                {
                    continue;
                }
                thread::park();
            }
        })
    };
    let _ = shared.timer.set(timer.thread().clone());
    let handles: Vec<_> = deadlines
        .iter()
        .enumerate()
        .map(|(me, &d)| {
            let s = shared.clone();
            thread::spawn(move || s.submit(me, d))
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    timer.join().unwrap();
}
