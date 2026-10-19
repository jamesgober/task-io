//! Prototype dispatchers for the R&D experiments (dev/RESEARCH.md). They run
//! as contenders so every experiment is measured by the same suites, against
//! the same competitors.
//!
//! This is experiment code. task-io's implementation is written separately
//! under its own rules (loom models, Miri, DIRECTIVES section 2); nothing here
//! is copied into `src/` without that.
//!
//! `proto-ring` tests, together:
//! - I-01: a fixed ring of job slots (Vyukov's bounded MPMC queue) instead of
//!   boxed jobs in a channel;
//! - I-16 / I-36: closures of up to 64 bytes stored inline in the slot, so
//!   dispatch allocates nothing;
//! - I-04: parking with an idle-worker bitmap and thread park tokens, so no
//!   wakeup is lost, with at most one woken worker searching at a time;
//! - I-05: a short spin, then yields, before a worker parks;
//! - I-27: per-worker health counters written with plain relaxed stores.

use std::cell::UnsafeCell;
use std::mem::{MaybeUninit, align_of, size_of};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering, fence};
use std::thread::{JoinHandle, Thread};

use crate::contenders::Contender;

// ---------------------------------------------------------------------------
// SmallJob: a closure stored inline when it fits in 64 bytes (I-16), boxed
// otherwise.
// ---------------------------------------------------------------------------

const INLINE: usize = 64;

#[repr(C, align(16))]
struct Storage([MaybeUninit<u8>; INLINE]);

struct SmallJob {
    data: Storage,
    /// Runs the closure stored in `data` and consumes it.
    call: unsafe fn(*mut u8),
}

// SAFETY: a SmallJob is only built from an `F: Send`, so moving it to another
// thread moves an `F` there.
unsafe impl Send for SmallJob {}

impl SmallJob {
    fn new<F: FnOnce() + Send + 'static>(f: F) -> Self {
        let mut data = Storage([MaybeUninit::uninit(); INLINE]);
        if size_of::<F>() <= INLINE && align_of::<F>() <= 16 {
            // SAFETY: `data` is 64 bytes, 16-aligned, and `F` fits in both.
            unsafe { ptr::write(data.0.as_mut_ptr().cast::<F>(), f) };
            unsafe fn call_inline<F: FnOnce()>(p: *mut u8) {
                // SAFETY: `p` points at an `F` written by `new` and not yet
                // read; reading it moves it out exactly once.
                let f = unsafe { ptr::read(p.cast::<F>()) };
                f();
            }
            Self {
                data,
                call: call_inline::<F>,
            }
        } else {
            let b: Box<F> = Box::new(f);
            // SAFETY: a pointer always fits in 64 bytes.
            unsafe { ptr::write(data.0.as_mut_ptr().cast::<*mut F>(), Box::into_raw(b)) };
            unsafe fn call_boxed<F: FnOnce()>(p: *mut u8) {
                // SAFETY: `p` holds a `*mut F` from `Box::into_raw`, read once.
                let raw = unsafe { ptr::read(p.cast::<*mut F>()) };
                let f = unsafe { Box::from_raw(raw) };
                f();
            }
            Self {
                data,
                call: call_boxed::<F>,
            }
        }
    }

    fn run(mut self) {
        // SAFETY: `call` matches how `data` was written in `new`, and `self`
        // is consumed, so the closure runs exactly once. (A prototype: a job
        // dropped without running leaks its closure; the real type drops it.)
        unsafe { (self.call)(self.data.0.as_mut_ptr().cast::<u8>()) }
    }
}

// ---------------------------------------------------------------------------
// Vyukov bounded MPMC ring.
// ---------------------------------------------------------------------------

#[repr(align(128))]
struct Padded<T>(T);

struct Cell {
    seq: AtomicUsize,
    job: UnsafeCell<MaybeUninit<SmallJob>>,
}

struct Ring {
    cells: Box<[Cell]>,
    mask: usize,
    head: Padded<AtomicUsize>, // next to dequeue
    tail: Padded<AtomicUsize>, // next to enqueue
}

// SAFETY: each cell's `job` is accessed only by the thread that won the CAS
// on its position, and `seq` orders the handoff (Vyukov's protocol).
unsafe impl Sync for Ring {}

impl Ring {
    fn new(capacity: usize) -> Self {
        let cap = capacity.next_power_of_two();
        let cells = (0..cap)
            .map(|i| Cell {
                seq: AtomicUsize::new(i),
                job: UnsafeCell::new(MaybeUninit::uninit()),
            })
            .collect();
        Self {
            cells,
            mask: cap - 1,
            head: Padded(AtomicUsize::new(0)),
            tail: Padded(AtomicUsize::new(0)),
        }
    }

    /// Returns the job back when the ring is full.
    fn push(&self, job: SmallJob) -> Result<(), SmallJob> {
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
                        // SAFETY: winning the CAS gives this thread the cell
                        // until `seq` is published below.
                        unsafe { (*cell.job.get()).write(job) };
                        cell.seq.store(pos + 1, Ordering::Release);
                        return Ok(());
                    }
                    Err(p) => pos = p,
                }
            } else if diff < 0 {
                return Err(job);
            } else {
                pos = self.tail.0.load(Ordering::Relaxed);
            }
        }
    }

    /// True when no job is waiting (a hint: it can change at once).
    fn is_empty(&self) -> bool {
        self.head.0.load(Ordering::SeqCst) >= self.tail.0.load(Ordering::SeqCst)
    }

    fn pop(&self) -> Option<SmallJob> {
        let mut pos = self.head.0.load(Ordering::Relaxed);
        loop {
            let cell = &self.cells[pos & self.mask];
            let seq = cell.seq.load(Ordering::Acquire);
            let diff = seq as isize - (pos + 1) as isize;
            if diff == 0 {
                match self.head.0.compare_exchange_weak(
                    pos,
                    pos + 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // SAFETY: the producer published this cell (`seq ==
                        // pos + 1`) and winning the CAS makes this thread its
                        // only reader.
                        let job = unsafe { (*cell.job.get()).assume_init_read() };
                        cell.seq.store(pos + self.mask + 1, Ordering::Release);
                        return Some(job);
                    }
                    Err(p) => pos = p,
                }
            } else if diff < 0 {
                return None;
            } else {
                pos = self.head.0.load(Ordering::Relaxed);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The pool.
// ---------------------------------------------------------------------------

/// Per-worker health record (I-27): written only by its worker.
#[repr(align(128))]
struct Health {
    jobs: AtomicU64,
    panics: AtomicU64,
}

struct Shared {
    ring: Ring,
    /// Bit `i` set = worker `i` is parked or about to park.
    idle: Padded<AtomicU64>,
    /// Workers woken and not yet holding a job. At most one wake is in flight
    /// at a time, so a fast submitter does not make one `unpark` per job.
    searching: Padded<AtomicUsize>,
    threads: Vec<Thread>,
    health: Box<[Health]>,
    stop: AtomicBool,
}

pub struct ProtoRing {
    shared: Arc<Shared>,
    handles: Vec<JoinHandle<()>>,
}

pub const PROTO_CAPACITY: usize = 65_536;

impl Shared {
    /// Clears one idle bit and unparks that worker. Returns false if no
    /// worker was idle.
    #[inline]
    fn wake_one(&self) -> bool {
        let mut idle = self.idle.0.load(Ordering::SeqCst);
        while idle != 0 {
            let bit = idle.trailing_zeros() as usize;
            match self.idle.0.compare_exchange_weak(
                idle,
                idle & !(1 << bit),
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    self.threads[bit].unpark();
                    return true;
                }
                Err(v) => idle = v,
            }
        }
        false
    }

    /// Called after work was published: wakes a worker only if none is
    /// already searching and one is idle.
    #[inline]
    fn notify(&self) {
        fence(Ordering::SeqCst);
        if self.idle.0.load(Ordering::Relaxed) == 0 {
            return;
        }
        if self
            .searching
            .0
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
            && !self.wake_one()
        {
            self.searching.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// A woken worker found a job (or gave up): it stops searching, and, if
    /// more work is waiting, hands the search to the next idle worker.
    #[inline]
    fn stop_searching(&self) {
        self.searching.0.fetch_sub(1, Ordering::SeqCst);
        fence(Ordering::SeqCst);
        if !self.ring.is_empty() {
            self.notify();
        }
    }
}

#[inline]
fn run_one(shared: &Shared, me: usize, job: SmallJob) {
    if catch_unwind(AssertUnwindSafe(|| job.run())).is_err() {
        let h = &shared.health[me].panics;
        h.store(h.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    }
    let h = &shared.health[me].jobs;
    h.store(h.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
}

/// Spins, then yields, re-checking the ring between steps (I-05), before a
/// worker parks. Returns a job if one arrived.
fn spin_for_job(shared: &Shared) -> Option<SmallJob> {
    for step in 0..10u32 {
        if step < 6 {
            for _ in 0..(1u32 << step) {
                std::hint::spin_loop();
            }
        } else {
            std::thread::yield_now();
        }
        if let Some(job) = shared.ring.pop() {
            return Some(job);
        }
    }
    None
}

fn worker(shared: Arc<Shared>, me: usize) {
    let bit = 1u64 << me;
    // Woken workers start out searching (the waker counted them).
    let mut searching = false;
    loop {
        while let Some(job) = shared.ring.pop() {
            if searching {
                searching = false;
                shared.stop_searching();
            }
            run_one(&shared, me, job);
        }
        if let Some(job) = spin_for_job(&shared) {
            if searching {
                searching = false;
                shared.stop_searching();
            }
            run_one(&shared, me, job);
            continue;
        }
        if searching {
            // Nothing found: give up the search, then re-check, so a job
            // published while we were counted as searching is not stranded.
            searching = false;
            shared.searching.0.fetch_sub(1, Ordering::SeqCst);
            fence(Ordering::SeqCst);
            if let Some(job) = shared.ring.pop() {
                run_one(&shared, me, job);
                continue;
            }
        }
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        // Announce idleness, then re-check: a push that happened before the
        // announcement is seen by the re-check; one after it sees the bit and
        // wakes us (the park token makes an early unpark stick).
        shared.idle.0.fetch_or(bit, Ordering::SeqCst);
        fence(Ordering::SeqCst);
        if let Some(job) = shared.ring.pop() {
            // If a waker already cleared our bit, it counted us as searching;
            // give that back, or the count leaks and nobody is woken again.
            let was_idle = shared.idle.0.fetch_and(!bit, Ordering::SeqCst) & bit != 0;
            if !was_idle {
                shared.stop_searching();
            }
            run_one(&shared, me, job);
            continue;
        }
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        std::thread::park();
        // If our bit is gone, a waker cleared it and counted us as searching.
        let was_idle = shared.idle.0.fetch_and(!bit, Ordering::SeqCst) & bit != 0;
        searching = !was_idle;
    }
}

impl Contender for ProtoRing {
    const NAME: &'static str = "proto-ring";

    fn start(workers: usize) -> Self {
        assert!(
            (1..=64).contains(&workers),
            "prototype supports 1 to 64 workers"
        );
        let shared_slot: Arc<std::sync::OnceLock<Arc<Shared>>> =
            Arc::new(std::sync::OnceLock::new());
        let mut handles = Vec::with_capacity(workers);
        let mut threads = Vec::with_capacity(workers);
        let ready = Arc::new(std::sync::Barrier::new(workers + 1));
        for me in 0..workers {
            let slot = shared_slot.clone();
            let ready = ready.clone();
            let h = std::thread::spawn(move || {
                ready.wait();
                let shared = slot.get().expect("shared set before release").clone();
                worker(shared, me);
            });
            threads.push(h.thread().clone());
            handles.push(h);
        }
        let shared = Arc::new(Shared {
            ring: Ring::new(PROTO_CAPACITY),
            idle: Padded(AtomicU64::new(0)),
            searching: Padded(AtomicUsize::new(0)),
            threads,
            health: (0..workers)
                .map(|_| Health {
                    jobs: AtomicU64::new(0),
                    panics: AtomicU64::new(0),
                })
                .collect(),
            stop: AtomicBool::new(false),
        });
        let _ = shared_slot.set(shared.clone());
        ready.wait();
        Self { shared, handles }
    }

    fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
        let mut job = SmallJob::new(f);
        loop {
            match self.shared.ring.push(job) {
                Ok(()) => break,
                Err(j) => {
                    // Full: back-pressure. Make sure someone is draining.
                    job = j;
                    self.shared.notify();
                    std::thread::yield_now();
                }
            }
        }
        self.shared.notify();
    }

    fn try_spawn<F: FnOnce() + Send + 'static>(&self, f: F) -> bool {
        if self.shared.ring.push(SmallJob::new(f)).is_err() {
            return false;
        }
        self.shared.notify();
        true
    }

    fn reserved_bytes_per_slot() -> Option<usize> {
        Some(size_of::<Cell>())
    }

    fn shutdown(self) {
        self.shared.stop.store(true, Ordering::Release);
        for t in &self.shared.threads {
            t.unpark();
        }
        for h in self.handles {
            let _ = h.join();
        }
    }
}
