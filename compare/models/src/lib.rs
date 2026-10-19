//! The wake protocol of `proto-ring` and `proto-shards` (`compare/src/proto.rs`),
//! reduced to its parts so loom can explore every interleaving: an idle
//! bitmap, a count of searching workers, park and unpark, and a job counter
//! standing in for the queue (the queue's own protocol is a separate model).
//!
//! The property checked is liveness: every published job is run, so no
//! interleaving ends with jobs waiting and every thread parked (loom reports
//! that as a deadlock).

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering, fence};
use loom::thread::{self, Thread};

/// Which version of the protocol to run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Variant {
    /// As in `compare/src/proto.rs` today.
    Fixed,
    /// Without the fix for the lost wakeup found on 2026-10-19 (RESEARCH
    /// section 6): a worker whose idle re-check finds a job does not give back
    /// the search it was counted for, so the count leaks.
    LeakOnRecheck,
}

pub struct Shared {
    variant: Variant,
    jobs: AtomicUsize,
    idle: AtomicU64,
    searching: AtomicUsize,
    stop: AtomicBool,
    done: AtomicUsize,
    target: usize,
    threads: loom::sync::Mutex<Vec<Thread>>,
    main: Thread,
}

impl Shared {
    fn pop(&self) -> bool {
        let mut n = self.jobs.load(Ordering::Acquire);
        while n > 0 {
            match self
                .jobs
                .compare_exchange(n, n - 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(v) => n = v,
            }
        }
        false
    }

    fn is_empty(&self) -> bool {
        self.jobs.load(Ordering::SeqCst) == 0
    }

    fn wake_one(&self) -> bool {
        let mut idle = self.idle.load(Ordering::SeqCst);
        while idle != 0 {
            let bit = idle.trailing_zeros() as usize;
            match self.idle.compare_exchange(
                idle,
                idle & !(1 << bit),
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    let t = self.threads.lock().unwrap()[bit].clone();
                    t.unpark();
                    return true;
                }
                Err(v) => idle = v,
            }
        }
        false
    }

    /// Called after a job is published.
    pub fn notify(&self) {
        fence(Ordering::SeqCst);
        if self.idle.load(Ordering::Relaxed) == 0 {
            return;
        }
        if self
            .searching
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
            && !self.wake_one()
        {
            self.searching.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn stop_searching(&self) {
        self.searching.fetch_sub(1, Ordering::SeqCst);
        fence(Ordering::SeqCst);
        if !self.is_empty() {
            self.notify();
        }
    }

    fn run(&self) {
        if self.done.fetch_add(1, Ordering::SeqCst) + 1 == self.target {
            self.main.unpark();
        }
    }

    /// The worker loop of `compare/src/proto.rs`, without the spin phase
    /// (spinning only re-checks the queue, which the model already does
    /// before parking).
    pub fn worker(&self, me: usize) {
        let bit = 1u64 << me;
        let mut searching = false;
        loop {
            while self.pop() {
                if searching {
                    searching = false;
                    self.stop_searching();
                }
                self.run();
            }
            if searching {
                searching = false;
                self.searching.fetch_sub(1, Ordering::SeqCst);
                fence(Ordering::SeqCst);
                if self.pop() {
                    self.run();
                    continue;
                }
            }
            if self.stop.load(Ordering::Acquire) {
                return;
            }
            self.idle.fetch_or(bit, Ordering::SeqCst);
            fence(Ordering::SeqCst);
            if self.pop() {
                let was_idle = self.idle.fetch_and(!bit, Ordering::SeqCst) & bit != 0;
                if !was_idle && self.variant == Variant::Fixed {
                    self.stop_searching();
                }
                self.run();
                continue;
            }
            if self.stop.load(Ordering::Acquire) {
                return;
            }
            thread::park();
            let was_idle = self.idle.fetch_and(!bit, Ordering::SeqCst) & bit != 0;
            searching = !was_idle;
        }
    }
}

/// One execution: `workers` workers, and the calling thread publishing
/// `jobs` jobs, then waiting (parked) until all have run, then stopping the
/// pool.
pub fn run_model(variant: Variant, workers: usize, jobs: usize) {
    let shared = Arc::new(Shared {
        variant,
        jobs: AtomicUsize::new(0),
        idle: AtomicU64::new(0),
        searching: AtomicUsize::new(0),
        stop: AtomicBool::new(false),
        done: AtomicUsize::new(0),
        target: jobs,
        threads: loom::sync::Mutex::new(Vec::new()),
        main: thread::current(),
    });
    let mut handles = Vec::new();
    for me in 0..workers {
        let s = shared.clone();
        handles.push(thread::spawn(move || s.worker(me)));
    }
    {
        let mut t = shared.threads.lock().unwrap();
        for h in &handles {
            t.push(h.thread().clone());
        }
    }
    for _ in 0..jobs {
        shared.jobs.fetch_add(1, Ordering::SeqCst);
        shared.notify();
    }
    while shared.done.load(Ordering::SeqCst) < jobs {
        thread::park();
    }
    shared.stop.store(true, Ordering::Release);
    for t in shared.threads.lock().unwrap().iter() {
        t.unpark();
    }
    for h in handles {
        h.join().unwrap();
    }
}
