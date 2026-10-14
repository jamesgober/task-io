//! Suite D-01 (dev/BENCHMARKS.md section 4.2): 1,000,000 jobs delayed
//! uniformly over a window, half of them cancelled.
//!
//! Contenders:
//! - `heap-scheduler`: the hand-rolled pattern, a scheduler thread over a
//!   `BinaryHeap` with a condvar, lazy cancellation, and due jobs handed to a
//!   crossbeam pool.
//! - `tokio-timer`: one task per job that awaits `sleep_until` (tokio docs,
//!   `tokio::time::sleep_until`), cancelled through its `AbortHandle`.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::contenders::{Contender, CrossbeamPool};

pub struct DelayResult {
    pub ns_per_submit: f64,
    pub ns_per_cancel: f64,
    pub lateness: hdrhistogram::Histogram<u64>,
    pub early: u64,
    pub fired_cancelled: u64,
}

/// Delays spread uniformly over `window`, in a fixed pseudo-random order.
fn delays(n: usize, window: Duration) -> Vec<Duration> {
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            Duration::from_nanos(x % window.as_nanos() as u64)
        })
        .collect()
}

struct Outcome {
    /// Lateness + 1 ns for each fired job; 0 if it never fired.
    late: Box<[AtomicU64]>,
    early: AtomicU64,
    fired_cancelled: AtomicU64,
    fired: AtomicU64,
}

impl Outcome {
    fn new(n: usize) -> &'static Self {
        Box::leak(Box::new(Self {
            late: (0..n).map(|_| AtomicU64::new(0)).collect(),
            early: AtomicU64::new(0),
            fired_cancelled: AtomicU64::new(0),
            fired: AtomicU64::new(0),
        }))
    }

    fn record(&self, i: usize, due: Instant, cancelled: bool) {
        let now = Instant::now();
        if now < due {
            self.early.fetch_add(1, Ordering::Relaxed);
        }
        if cancelled {
            self.fired_cancelled.fetch_add(1, Ordering::Relaxed);
        }
        let late = now.saturating_duration_since(due).as_nanos() as u64;
        self.late[i].store(late + 1, Ordering::Relaxed);
        self.fired.fetch_add(1, Ordering::Relaxed);
    }

    fn finish(&self, submit: Duration, cancel: Duration, n: usize, cancels: usize) -> DelayResult {
        let mut h = crate::stats::histogram();
        for s in self.late.iter() {
            let v = s.load(Ordering::Relaxed);
            if v > 0 {
                let _ = h.record(v);
            }
        }
        DelayResult {
            ns_per_submit: submit.as_nanos() as f64 / n as f64,
            ns_per_cancel: cancel.as_nanos() as f64 / cancels.max(1) as f64,
            lateness: h,
            early: self.early.load(Ordering::Relaxed),
            fired_cancelled: self.fired_cancelled.load(Ordering::Relaxed),
        }
    }
}

/// Waits until `target` jobs fired, or `limit` passed.
fn wait_fired(o: &Outcome, target: u64, limit: Duration) {
    let t = Instant::now();
    while o.fired.load(Ordering::Relaxed) < target && t.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---------------------------------------------------------------------------
// heap-scheduler
// ---------------------------------------------------------------------------

struct Heap {
    q: Mutex<BinaryHeap<Reverse<(Instant, usize)>>>,
    cv: Condvar,
    stop: AtomicBool,
}

pub fn d01_heap(workers: usize, n: usize, window: Duration) -> DelayResult {
    let pool = Arc::new(CrossbeamPool::start(workers));
    let out = Outcome::new(n);
    let cancelled: &'static [AtomicBool] =
        Box::leak((0..n).map(|_| AtomicBool::new(false)).collect::<Box<[_]>>());
    let heap: &'static Heap = Box::leak(Box::new(Heap {
        q: Mutex::new(BinaryHeap::with_capacity(n)),
        cv: Condvar::new(),
        stop: AtomicBool::new(false),
    }));
    let sched = {
        let pool = pool.clone();
        std::thread::spawn(move || {
            let mut q = heap.q.lock().expect("heap");
            loop {
                if heap.stop.load(Ordering::Relaxed) {
                    break;
                }
                match q.peek().copied() {
                    None => q = heap.cv.wait(q).expect("heap"),
                    Some(Reverse((due, i))) => {
                        let now = Instant::now();
                        if due <= now {
                            q.pop();
                            if !cancelled[i].load(Ordering::Relaxed) {
                                pool.spawn(move || {
                                    out.record(i, due, cancelled[i].load(Ordering::Relaxed))
                                });
                            }
                        } else {
                            q = heap.cv.wait_timeout(q, due - now).expect("heap").0;
                        }
                    }
                }
            }
        })
    };
    let ds = delays(n, window);
    let t0 = Instant::now();
    for (i, d) in ds.iter().enumerate() {
        let due = t0 + *d;
        let mut q = heap.q.lock().expect("heap");
        let earliest = q.peek().is_none_or(|Reverse((e, _))| due < *e);
        q.push(Reverse((due, i)));
        drop(q);
        if earliest {
            heap.cv.notify_one();
        }
    }
    let submit = t0.elapsed();
    let tc = Instant::now();
    for c in cancelled.iter().step_by(2) {
        c.store(true, Ordering::Relaxed);
    }
    let cancel = tc.elapsed();
    let expected = (n - n.div_ceil(2)) as u64;
    wait_fired(out, expected, window + Duration::from_secs(30));
    heap.stop.store(true, Ordering::Relaxed);
    heap.cv.notify_all();
    let _ = sched.join();
    if let Ok(p) = Arc::try_unwrap(pool) {
        p.shutdown();
    }
    out.finish(submit, cancel, n, n.div_ceil(2))
}

// ---------------------------------------------------------------------------
// tokio-timer
// ---------------------------------------------------------------------------

pub fn d01_tokio(workers: usize, n: usize, window: Duration) -> DelayResult {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_time()
        .build()
        .expect("tokio runtime");
    let out = Outcome::new(n);
    // The same cancelled flags as the heap scheduler, so a job that runs after
    // its cancel is counted the same way for both.
    let cancelled: &'static [AtomicBool] =
        Box::leak((0..n).map(|_| AtomicBool::new(false)).collect::<Box<[_]>>());
    let ds = delays(n, window);
    let mut handles = Vec::with_capacity(n);
    let t0 = Instant::now();
    let base = tokio::time::Instant::from_std(t0);
    for (i, d) in ds.iter().enumerate() {
        let due = t0 + *d;
        let tdue = base + *d;
        handles.push(rt.spawn(async move {
            tokio::time::sleep_until(tdue).await;
            out.record(i, due, cancelled[i].load(Ordering::Relaxed));
        }));
    }
    let submit = t0.elapsed();
    let tc = Instant::now();
    for (i, h) in handles.iter().enumerate().step_by(2) {
        cancelled[i].store(true, Ordering::Relaxed);
        h.abort();
    }
    let cancel = tc.elapsed();
    let expected = (n - n.div_ceil(2)) as u64;
    wait_fired(out, expected, window + Duration::from_secs(30));
    drop(handles);
    rt.shutdown_timeout(Duration::from_secs(10));
    out.finish(submit, cancel, n, n.div_ceil(2))
}
