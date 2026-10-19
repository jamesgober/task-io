//! The bounded MPMC ring of `compare/src/proto.rs` (Vyukov's protocol), with
//! loom's `UnsafeCell`, so loom checks every cell access for a data race
//! (ARCHITECTURE section 11.1, Q-1). Values are `u64`s here; the prototype's
//! cells hold inline closures, whose own handling is Q-2 (a Miri check).
//!
//! Two changes from the prototype, both needed for loom and neither changing
//! the protocol: a strong `compare_exchange` (loom lets a weak one fail
//! spuriously without end), and `loom::hint::spin_loop` where a thread
//! re-reads a position it lost, so loom can schedule the thread it waits on.

use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicUsize, Ordering};

struct Cell {
    seq: AtomicUsize,
    value: UnsafeCell<u64>,
}

pub struct Ring {
    cells: Box<[Cell]>,
    mask: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
}

// SAFETY: a cell's value is touched only by the thread that won the CAS on
// its position, and `seq` hands it over (the protocol under test).
unsafe impl Sync for Ring {}

impl Ring {
    /// A ring of at least 2 cells. With 1 cell the protocol breaks: a popped
    /// cell's `seq` becomes `pos + mask + 1 = pos + 1`, the value a published
    /// cell has, so the next push sees it as free and overwrites a value that
    /// was never popped, and a consumer can then spin without end
    /// (`tests/ring.rs` shows loom finding both).
    pub fn new(capacity: usize) -> Self {
        Self::new_unclamped(capacity.max(2))
    }

    /// `new` without the lower bound, for the test that shows why it exists.
    pub fn new_unclamped(capacity: usize) -> Self {
        let cap = capacity.next_power_of_two();
        Self {
            cells: (0..cap)
                .map(|i| Cell {
                    seq: AtomicUsize::new(i),
                    value: UnsafeCell::new(0),
                })
                .collect(),
            mask: cap - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Returns the value back when the ring is full.
    pub fn push(&self, v: u64) -> Result<(), u64> {
        let mut pos = self.tail.load(Ordering::Relaxed);
        loop {
            let cell = &self.cells[pos & self.mask];
            let seq = cell.seq.load(Ordering::Acquire);
            let diff = seq as isize - pos as isize;
            if diff == 0 {
                match self
                    .tail
                    .compare_exchange(pos, pos + 1, Ordering::Relaxed, Ordering::Relaxed)
                {
                    Ok(_) => {
                        // SAFETY: winning the CAS while `seq == pos` makes
                        // this thread the cell's only writer until the store
                        // below publishes it.
                        cell.value.with_mut(|p| unsafe { *p = v });
                        cell.seq.store(pos + 1, Ordering::Release);
                        return Ok(());
                    }
                    Err(p) => pos = p,
                }
            } else if diff < 0 {
                return Err(v);
            } else {
                loom::hint::spin_loop();
                pos = self.tail.load(Ordering::Relaxed);
            }
        }
    }

    pub fn pop(&self) -> Option<u64> {
        let mut pos = self.head.load(Ordering::Relaxed);
        loop {
            let cell = &self.cells[pos & self.mask];
            let seq = cell.seq.load(Ordering::Acquire);
            let diff = seq as isize - (pos + 1) as isize;
            if diff == 0 {
                match self
                    .head
                    .compare_exchange(pos, pos + 1, Ordering::Relaxed, Ordering::Relaxed)
                {
                    Ok(_) => {
                        // SAFETY: the producer published this cell
                        // (`seq == pos + 1`, read with `Acquire`), and winning
                        // the CAS makes this thread its only reader.
                        let v = cell.value.with(|p| unsafe { *p });
                        cell.seq.store(pos + self.mask + 1, Ordering::Release);
                        return Some(v);
                    }
                    Err(p) => pos = p,
                }
            } else if diff < 0 {
                return None;
            } else {
                loom::hint::spin_loop();
                pos = self.head.load(Ordering::Relaxed);
            }
        }
    }
}
