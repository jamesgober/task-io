//! A counting global allocator for the memory suites.
//!
//! Counting is off by default, so timing suites pay one relaxed load per
//! allocation and nothing else. Memory suites switch it on around the part
//! they measure.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::Relaxed};

pub struct Counting;

static ON: AtomicBool = AtomicBool::new(false);
static LIVE_BYTES: AtomicI64 = AtomicI64::new(0);
static ALLOCS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwards the caller's layout unchanged to the system allocator.
        let p = unsafe { System.alloc(layout) };
        if ON.load(Relaxed) && !p.is_null() {
            LIVE_BYTES.fetch_add(layout.size() as i64, Relaxed);
            ALLOCS.fetch_add(1, Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ON.load(Relaxed) {
            LIVE_BYTES.fetch_sub(layout.size() as i64, Relaxed);
        }
        // SAFETY: `ptr` came from `alloc` above with this same layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: same contract as GlobalAlloc::realloc, forwarded unchanged.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if ON.load(Relaxed) && !p.is_null() {
            LIVE_BYTES.fetch_add(new_size as i64 - layout.size() as i64, Relaxed);
            ALLOCS.fetch_add(1, Relaxed);
        }
        p
    }
}

/// Snapshot of the counters.
#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    pub live_bytes: i64,
    pub allocs: u64,
}

pub fn start() {
    ON.store(true, Relaxed);
}

pub fn stop() {
    ON.store(false, Relaxed);
}

pub fn snapshot() -> Snapshot {
    Snapshot {
        live_bytes: LIVE_BYTES.load(Relaxed),
        allocs: ALLOCS.load(Relaxed),
    }
}
