//! Experiment I-43 (dev/RESEARCH.md): delayed submits that never take the
//! timer's lock. Each submitter pushes (deadline, payload) into its own
//! bounded ring; the timer thread, which owns the wheel outright, drains the
//! rings when it wakes.
//!
//! A ring is Vyukov's bounded queue (as in `proto.rs`) with one consumer, so
//! the consumer's side needs no compare-exchange. Its fields are atomics, so
//! there is no `unsafe`: the producer writes the value with relaxed stores
//! and publishes it with a release store of the cell's sequence; the consumer
//! reads the sequence with acquire before it reads the value.
//!
//! Wake protocol (the timer sleeps toward `sleeping_until`, in ticks):
//! - timer: drain, fire, compute the next deadline, store it (SeqCst), then
//!   re-check the rings; park only if they are empty.
//! - submit: push, fence (SeqCst), load the target; if the new deadline is
//!   earlier, lower the target with a compare-exchange and unpark the timer
//!   when that wins. Either the timer's re-check sees the push or the submit
//!   sees the target the timer is about to sleep toward (the store-load
//!   pattern of the worker wake protocol, `compare/models`).
//! - pressure: every half ring of pushes also unparks the timer, so bursts
//!   of later deadlines are drained before the rings fill.

use std::cell::Cell as StdCell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering, fence};
use std::thread::Thread;

#[repr(align(128))]
struct Padded<T>(T);

struct Cell {
    seq: AtomicUsize,
    deadline: AtomicU64,
    payload: AtomicU64,
}

struct Ring {
    cells: Box<[Cell]>,
    mask: usize,
    /// Next position to pop; only the consumer moves it.
    head: Padded<AtomicUsize>,
    tail: Padded<AtomicUsize>,
}

/// The outcome of a push that was accepted.
enum Pushed {
    Done,
    /// This push completed another half ring of pushes: wake the consumer.
    HalfFull,
}

impl Ring {
    fn new(capacity: usize) -> Self {
        // At least 2 cells (a 1-cell Vyukov ring overwrites unread values;
        // compare/models/tests/ring.rs).
        let cap = capacity.max(2).next_power_of_two();
        Self {
            cells: (0..cap)
                .map(|i| Cell {
                    seq: AtomicUsize::new(i),
                    deadline: AtomicU64::new(0),
                    payload: AtomicU64::new(0),
                })
                .collect(),
            mask: cap - 1,
            head: Padded(AtomicUsize::new(0)),
            tail: Padded(AtomicUsize::new(0)),
        }
    }

    fn push(&self, deadline: u64, payload: u64) -> Option<Pushed> {
        let mut pos = self.tail.0.load(Ordering::Relaxed);
        loop {
            let cell = &self.cells[pos & self.mask];
            let seq = cell.seq.load(Ordering::Acquire);
            let diff = seq as isize - pos as isize;
            if diff == 0 {
                match self.tail.0.compare_exchange_weak(
                    pos,
                    pos + 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        cell.deadline.store(deadline, Ordering::Relaxed);
                        cell.payload.store(payload, Ordering::Relaxed);
                        cell.seq.store(pos + 1, Ordering::Release);
                        // Every half ring of pushes (by position, so no
                        // read of the consumer's head).
                        let half = self.cells.len() / 2;
                        return Some(if pos % half == half - 1 {
                            Pushed::HalfFull
                        } else {
                            Pushed::Done
                        });
                    }
                    Err(p) => pos = p,
                }
            } else if diff < 0 {
                return None;
            } else {
                pos = self.tail.0.load(Ordering::Relaxed);
            }
        }
    }

    /// Single consumer.
    fn pop(&self) -> Option<(u64, u64)> {
        let pos = self.head.0.load(Ordering::Relaxed);
        let cell = &self.cells[pos & self.mask];
        if cell.seq.load(Ordering::Acquire) != pos + 1 {
            return None;
        }
        let v = (
            cell.deadline.load(Ordering::Relaxed),
            cell.payload.load(Ordering::Relaxed),
        );
        cell.seq.store(pos + self.mask + 1, Ordering::Release);
        self.head.0.store(pos + 1, Ordering::Relaxed);
        Some(v)
    }

    fn is_empty(&self) -> bool {
        let pos = self.head.0.load(Ordering::Relaxed);
        self.cells[pos & self.mask].seq.load(Ordering::SeqCst) != pos + 1
    }
}

static NEXT_INTAKE: AtomicUsize = AtomicUsize::new(1);

thread_local! {
    /// (intake id, home ring) of the intake this thread last submitted to.
    static HOME: StdCell<(usize, usize)> = const { StdCell::new((0, 0)) };
}

pub struct Intake {
    rings: Box<[Ring]>,
    id: usize,
    next_home: AtomicUsize,
    /// The deadline the timer sleeps toward (`u64::MAX`: none).
    sleeping_until: Padded<AtomicU64>,
    /// How to wake the timer thread: `unpark`, or another primitive it
    /// sleeps on (the high-resolution timer of I-33 on Windows).
    waker: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl Intake {
    /// `rings` rings of `per_ring` entries each.
    pub fn new(rings: usize, per_ring: usize) -> Self {
        Self {
            rings: (0..rings.max(1)).map(|_| Ring::new(per_ring)).collect(),
            id: NEXT_INTAKE.fetch_add(1, Ordering::Relaxed),
            next_home: AtomicUsize::new(0),
            sleeping_until: Padded(AtomicU64::new(u64::MAX)),
            waker: OnceLock::new(),
        }
    }

    /// Called once by the timer thread before it first parks.
    pub fn set_timer(&self, t: Thread) {
        self.set_waker(Box::new(move || t.unpark()));
    }

    /// As `set_timer`, for a timer thread that sleeps on something else.
    pub fn set_waker(&self, w: Box<dyn Fn() + Send + Sync>) {
        let _ = self.waker.set(w);
    }

    fn wake(&self) {
        if let Some(w) = self.waker.get() {
            w();
        }
    }

    /// Wakes the timer thread however it sleeps (used at shutdown).
    pub fn wake_timer(&self) {
        self.wake();
    }

    fn home(&self) -> usize {
        HOME.with(|c| {
            let (id, ring) = c.get();
            if id == self.id {
                ring
            } else {
                let ring = self.next_home.fetch_add(1, Ordering::Relaxed) % self.rings.len();
                c.set((self.id, ring));
                ring
            }
        })
    }

    /// Hands a timer to the timer thread. Never takes a lock. When every
    /// ring is full the submit waits for the timer to drain (backpressure,
    /// bounded memory).
    pub fn submit(&self, deadline: u64, payload: u64) {
        self.submit_inner::<true>(deadline, payload);
    }

    /// `submit` without the SeqCst fence, which breaks the wake protocol
    /// (a submit can be left in a ring while the timer sleeps). Only for
    /// timing what the fence costs (I-43); never used by a contender.
    pub fn submit_without_fence(&self, deadline: u64, payload: u64) {
        self.submit_inner::<false>(deadline, payload);
    }

    #[inline]
    fn submit_inner<const FENCE: bool>(&self, deadline: u64, payload: u64) {
        let home = self.home();
        let n = self.rings.len();
        let pushed = 'push: loop {
            for k in 0..n {
                if let Some(p) = self.rings[(home + k) % n].push(deadline, payload) {
                    break 'push p;
                }
            }
            self.wake();
            std::thread::yield_now();
        };
        if let Pushed::HalfFull = pushed {
            self.wake();
        }
        if FENCE {
            fence(Ordering::SeqCst);
        }
        let mut s = self.sleeping_until.0.load(Ordering::Relaxed);
        while deadline < s {
            match self.sleeping_until.0.compare_exchange_weak(
                s,
                deadline,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.wake();
                    return;
                }
                Err(v) => s = v,
            }
        }
    }

    /// Timer side: moves every waiting entry to `f`. Returns how many.
    pub fn drain(&self, mut f: impl FnMut(u64, u64)) -> usize {
        let mut moved = 0;
        for r in self.rings.iter() {
            while let Some((d, p)) = r.pop() {
                f(d, p);
                moved += 1;
            }
        }
        moved
    }

    /// Timer side, before it parks: publishes the deadline it will sleep
    /// toward, then reports whether a submit slipped in (drain again if so).
    pub fn sleep_toward(&self, deadline: Option<u64>) -> bool {
        self.sleeping_until
            .0
            .store(deadline.unwrap_or(u64::MAX), Ordering::SeqCst);
        fence(Ordering::SeqCst);
        self.rings.iter().all(Ring::is_empty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn ring_is_fifo_and_bounded() {
        let r = Ring::new(4);
        for k in 0..4 {
            assert!(r.push(k, k * 10).is_some());
        }
        assert!(r.push(9, 9).is_none(), "accepted past capacity");
        for k in 0..4 {
            assert_eq!(r.pop(), Some((k, k * 10)));
        }
        assert_eq!(r.pop(), None);
        assert!(r.is_empty());
    }

    #[test]
    fn wakes_every_half_ring() {
        let r = Ring::new(8);
        let halves: Vec<u64> = (0..8)
            .filter(|&k| matches!(r.push(k, k), Some(Pushed::HalfFull)))
            .collect();
        assert_eq!(halves, vec![3, 7]);
    }

    /// Several submitters and the draining thread at once, with small rings
    /// so submits hit backpressure: every value arrives exactly once.
    #[test]
    fn every_submit_arrives_once() {
        const SUBMITTERS: u64 = 4;
        const PER: u64 = 50_000;
        let intake = Arc::new(Intake::new(2, 16));
        let drainer = {
            let intake = intake.clone();
            std::thread::spawn(move || {
                intake.set_timer(std::thread::current());
                let mut seen = vec![false; (SUBMITTERS * PER) as usize];
                let mut got = 0u64;
                while got < SUBMITTERS * PER {
                    intake.drain(|d, p| {
                        assert_eq!(d, p ^ 0xabcd, "torn entry");
                        assert!(!std::mem::replace(&mut seen[p as usize], true), "twice");
                        got += 1;
                    });
                    if intake.sleep_toward(None) {
                        std::thread::park_timeout(std::time::Duration::from_millis(1));
                    }
                }
                got
            })
        };
        let subs: Vec<_> = (0..SUBMITTERS)
            .map(|s| {
                let intake = intake.clone();
                std::thread::spawn(move || {
                    for k in 0..PER {
                        let p = s * PER + k;
                        intake.submit(p ^ 0xabcd, p);
                    }
                })
            })
            .collect();
        for s in subs {
            s.join().unwrap();
        }
        assert_eq!(drainer.join().unwrap(), SUBMITTERS * PER);
    }
}
