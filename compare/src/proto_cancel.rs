//! Experiment I-44 (dev/RESEARCH.md): cancel as one plain store, through a
//! handle that keeps its timer's slot from being reused while it exists.
//!
//! Each delayed timer gets a slot holding a cancelled flag and a pin count.
//! The handle holds one pin and the timer side holds the other; whoever
//! releases the last pin frees the slot. Because a handle always pins its
//! slot, no reuse can race its cancel, so cancel is a release store and
//! nothing else. The cost moves to the pin releases (one RMW each, at handle
//! drop and when the timer is done with the entry), which D-01 times
//! separately as "time per handle drop".
//!
//! Slots are allocated from a shared free list through a per-thread cache
//! of up to `BATCH` indexes, refilled and flushed in batches under a mutex,
//! so a submit or a release takes the lock once per `BATCH` slots. A cache
//! goes back to the shared list when its thread exits (or first uses another
//! table), so no slot is stranded.

use std::cell::RefCell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const BATCH: usize = 256;

pub struct Slots {
    /// One byte per slot, apart from the pins, so a run of cancels touches
    /// as little memory as a flag array does.
    cancelled: Box<[AtomicBool]>,
    pins: Box<[AtomicU32]>,
    free: Mutex<Vec<u32>>,
}

/// One thread's slot cache for one table: indexes to hand out, and freed
/// indexes waiting to go back.
struct Cache {
    slots: Option<&'static Slots>,
    take: Vec<u32>,
    back: Vec<u32>,
}

impl Cache {
    fn flush(&mut self) {
        if let Some(s) = self.slots {
            let mut free = s.free.lock().unwrap_or_else(|e| e.into_inner());
            free.append(&mut self.take);
            free.append(&mut self.back);
        }
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        self.flush();
    }
}

thread_local! {
    static CACHE: RefCell<Cache> = const {
        RefCell::new(Cache { slots: None, take: Vec::new(), back: Vec::new() })
    };
}

#[derive(Debug, PartialEq, Eq)]
pub struct Full;

impl Slots {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.min(u32::MAX as usize);
        Self {
            cancelled: (0..capacity).map(|_| AtomicBool::new(false)).collect(),
            pins: (0..capacity).map(|_| AtomicU32::new(0)).collect(),
            // Popped from the back: hand out low indexes first.
            free: Mutex::new((0..capacity as u32).rev().collect()),
        }
    }

    fn with_cache<R>(&'static self, f: impl FnOnce(&mut Cache) -> R) -> R {
        CACHE.with(|c| {
            let mut c = c.borrow_mut();
            if !c.slots.is_some_and(|s| std::ptr::eq(s, self)) {
                c.flush();
                c.slots = Some(self);
            }
            f(&mut c)
        })
    }

    /// A fresh slot pinned twice (handle and timer side).
    pub fn alloc(&'static self) -> Result<u32, Full> {
        let i = self.with_cache(|c| {
            if c.take.is_empty() {
                // Reuse this thread's own freed slots first, then the shared
                // list.
                std::mem::swap(&mut c.take, &mut c.back);
                if c.take.is_empty() {
                    let mut free = self.free.lock().expect("slots");
                    let n = free.len().min(BATCH);
                    let at = free.len() - n;
                    c.take.extend(free.drain(at..));
                }
            }
            c.take.pop()
        });
        let i = i.ok_or(Full)?;
        self.cancelled[i as usize].store(false, Ordering::Relaxed);
        // Release: the timer side reads the slot after the intake hands it
        // over, which already orders this; the handle is on this thread.
        self.pins[i as usize].store(2, Ordering::Release);
        Ok(i)
    }

    pub fn cancel(&self, i: u32) {
        self.cancelled[i as usize].store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self, i: u32) -> bool {
        self.cancelled[i as usize].load(Ordering::Acquire)
    }

    /// Drops one pin; the last one frees the slot.
    pub fn release(&'static self, i: u32) {
        if self.pins[i as usize].fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        self.with_cache(|c| {
            c.back.push(i);
            if c.back.len() >= 2 * BATCH {
                let mut free = self.free.lock().expect("slots");
                free.extend(c.back.drain(BATCH..));
            }
        });
    }
}

/// A cancellable timer: pins its slot until dropped.
pub struct Handle {
    slots: &'static Slots,
    slot: u32,
}

impl Handle {
    pub fn new(slots: &'static Slots, slot: u32) -> Self {
        Self { slots, slot }
    }

    /// One release store. Takes effect if the timer has not been handed to
    /// a worker yet; the worker checks it again before running.
    #[inline]
    pub fn cancel(&self) {
        self.slots.cancel(self.slot);
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.slots.release(self.slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn leak(n: usize) -> &'static Slots {
        Box::leak(Box::new(Slots::new(n)))
    }

    #[test]
    fn slot_is_reused_only_after_both_pins() {
        let s = leak(1);
        let a = s.alloc().unwrap();
        assert_eq!(s.alloc(), Err(Full));
        s.release(a); // timer side done
        assert_eq!(s.alloc(), Err(Full), "reused while the handle holds it");
        s.release(a); // handle dropped
        let b = s.alloc().unwrap();
        assert_eq!(a, b);
        assert!(!s.is_cancelled(b), "a reused slot kept the old cancel");
    }

    #[test]
    fn cancel_through_handle() {
        let s = leak(4);
        let i = s.alloc().unwrap();
        let h = Handle::new(s, i);
        h.cancel();
        assert!(s.is_cancelled(i));
        drop(h);
        s.release(i);
    }

    /// Threads allocate, cancel some, and release both pins from different
    /// threads, many times over a small table: no slot is handed out twice
    /// at once, and none is lost (the table can be filled again at the end,
    /// once every thread has exited).
    #[test]
    fn no_slot_is_handed_out_twice() {
        const THREADS: usize = 4;
        const ROUNDS: usize = 20_000;
        let cap = THREADS * 3 * BATCH;
        let s = leak(cap);
        let owner: &'static [AtomicUsize] =
            Box::leak((0..cap).map(|_| AtomicUsize::new(0)).collect::<Box<[_]>>());
        let (tx, rx) = std::sync::mpsc::channel::<u32>();
        // The "timer side": releases the second pin on another thread.
        let timer = std::thread::spawn(move || {
            for i in rx {
                s.release(i);
            }
        });
        let workers: Vec<_> = (0..THREADS)
            .map(|t| {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    for r in 0..ROUNDS {
                        let i = loop {
                            match s.alloc() {
                                Ok(i) => break i,
                                Err(Full) => std::thread::yield_now(),
                            }
                        };
                        let prev = owner[i as usize].swap(t + 1, Ordering::AcqRel);
                        assert_eq!(prev, 0, "slot {i} handed out twice");
                        let h = Handle::new(s, i);
                        if r % 2 == 0 {
                            h.cancel();
                        }
                        owner[i as usize].store(0, Ordering::Release);
                        tx.send(i).unwrap();
                        drop(h);
                    }
                })
            })
            .collect();
        drop(tx);
        for w in workers {
            w.join().unwrap();
        }
        timer.join().unwrap();
        // The exited threads' caches went back to the shared list, so every
        // slot can be taken again.
        let mut got = 0;
        while s.alloc().is_ok() {
            got += 1;
        }
        assert_eq!(got, cap, "slots lost");
    }
}
