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
    /// Time per cancel-handle drop, for contenders whose handle does work
    /// when dropped (`proto-wheel-handle`); not part of submit or cancel.
    pub ns_per_drop: Option<f64>,
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
            ns_per_drop: None,
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

// ---------------------------------------------------------------------------
// proto-wheel (experiment I-07)
// ---------------------------------------------------------------------------

/// The wheel and the deadline (in ticks) the scheduler sleeps toward, so a
/// submit wakes it only for an earlier one.
struct WheelState {
    wheel: crate::proto_wheel::Wheel,
    sleeping_until: u64,
}

struct WheelShared {
    state: Mutex<WheelState>,
    cv: Condvar,
    stop: AtomicBool,
}

/// `proto-wheel`: the heap scheduler's shape (one scheduler thread, a mutex
/// and condvar, due jobs handed to crossbeam-pool) over the timing wheel of
/// `proto_wheel.rs`, with 1 µs ticks. Deadlines round up to the next tick, so
/// nothing fires early; cancel removes the timer by id (the heap scheduler
/// only sets a flag and keeps the entry until it is due).
pub fn d01_wheel(workers: usize, n: usize, window: Duration) -> DelayResult {
    let pool = Arc::new(CrossbeamPool::start(workers));
    let out = Outcome::new(n);
    let cancelled: &'static [AtomicBool] =
        Box::leak((0..n).map(|_| AtomicBool::new(false)).collect::<Box<[_]>>());
    let ds: &'static [Duration] = Box::leak(delays(n, window).into_boxed_slice());
    let shared: &'static WheelShared = Box::leak(Box::new(WheelShared {
        state: Mutex::new(WheelState {
            wheel: crate::proto_wheel::Wheel::new(n),
            sleeping_until: u64::MAX,
        }),
        cv: Condvar::new(),
        stop: AtomicBool::new(false),
    }));
    let t0 = Instant::now();
    let tick = |d: Duration| d.as_nanos().div_ceil(1_000) as u64;
    let sched = {
        let pool = pool.clone();
        std::thread::spawn(move || {
            let mut g = shared.state.lock().expect("wheel");
            loop {
                if shared.stop.load(Ordering::Relaxed) {
                    break;
                }
                // Floor: a tick counts as reached only once it has fully passed.
                let now = (t0.elapsed().as_nanos() / 1_000) as u64;
                g.wheel.advance(now, |_, _, i| {
                    let i = i as usize;
                    let due = t0 + ds[i];
                    pool.spawn(move || out.record(i, due, cancelled[i].load(Ordering::Relaxed)));
                });
                match g.wheel.next_deadline() {
                    None => {
                        g.sleeping_until = u64::MAX;
                        g = shared.cv.wait(g).expect("wheel");
                    }
                    Some(d) => {
                        g.sleeping_until = d;
                        let target = t0 + Duration::from_micros(d);
                        let now = Instant::now();
                        if target > now {
                            g = shared.cv.wait_timeout(g, target - now).expect("wheel").0;
                        }
                    }
                }
            }
        })
    };
    let mut ids = Vec::with_capacity(n);
    let ts = Instant::now();
    for (i, d) in ds.iter().enumerate() {
        // The deadline relative to t0, which is taken before the loop as the
        // other contenders take theirs.
        let due = tick(*d);
        let mut g = shared.state.lock().expect("wheel");
        let id = g.wheel.insert(due, i as u64).expect("capacity is n");
        let wake = due < g.sleeping_until;
        if wake {
            g.sleeping_until = due;
        }
        drop(g);
        if wake {
            shared.cv.notify_one();
        }
        ids.push(id);
    }
    let submit = ts.elapsed();
    let tc = Instant::now();
    for (i, id) in ids.iter().enumerate().step_by(2) {
        cancelled[i].store(true, Ordering::Relaxed);
        shared.state.lock().expect("wheel").wheel.cancel(*id);
    }
    let cancel = tc.elapsed();
    let expected = (n - n.div_ceil(2)) as u64;
    wait_fired(out, expected, window + Duration::from_secs(30));
    shared.stop.store(true, Ordering::Relaxed);
    shared.cv.notify_all();
    let _ = sched.join();
    if let Ok(p) = Arc::try_unwrap(pool) {
        p.shutdown();
    }
    out.finish(submit, cancel, n, n.div_ceil(2))
}

// ---------------------------------------------------------------------------
// proto-wheel-intake (experiment I-43)
// ---------------------------------------------------------------------------

/// How a timer thread sleeps: `park` and `park_timeout`, or (I-33, Windows
/// only) a high-resolution waitable timer with a wake event; `park_timeout`
/// waits in whole timer periods on Windows (about 15.6 ms by default).
enum Sleeper {
    Park,
    #[cfg(windows)]
    HiRes(Arc<crate::sleep_probe::Wakeable>),
}

impl Sleeper {
    fn new(hires: bool) -> Self {
        #[cfg(windows)]
        if hires && let Some(w) = crate::sleep_probe::Wakeable::new() {
            return Sleeper::HiRes(Arc::new(w));
        }
        let _ = hires;
        Sleeper::Park
    }

    /// Hands the intake its way to wake this thread.
    fn register(&self, intake: &crate::proto_intake::Intake) {
        match self {
            Sleeper::Park => intake.set_timer(std::thread::current()),
            #[cfg(windows)]
            Sleeper::HiRes(w) => {
                let w = w.clone();
                intake.set_waker(Box::new(move || w.wake()));
            }
        }
    }

    fn sleep(&self, d: Option<Duration>) {
        match (self, d) {
            (Sleeper::Park, None) => std::thread::park(),
            (Sleeper::Park, Some(d)) => std::thread::park_timeout(d),
            #[cfg(windows)]
            (Sleeper::HiRes(w), d) => w.wait(d),
        }
    }
}

/// `proto-wheel-intake`: the timer thread owns the wheel with no lock;
/// submits go through `proto_intake::Intake` (per-submitter rings, a wake only
/// for an earlier deadline or a half ring of pushes). Cancel is the heap
/// scheduler's flag, checked when the timer fires, so this row differs from
/// heap-scheduler in submit only (I-44 is the cancel experiment).
pub fn d01_wheel_intake(workers: usize, n: usize, window: Duration, hires: bool) -> DelayResult {
    use crate::proto_intake::Intake;
    let pool = Arc::new(CrossbeamPool::start(workers));
    let out = Outcome::new(n);
    let cancelled: &'static [AtomicBool] =
        Box::leak((0..n).map(|_| AtomicBool::new(false)).collect::<Box<[_]>>());
    let ds: &'static [Duration] = Box::leak(delays(n, window).into_boxed_slice());
    let intake: &'static Intake = Box::leak(Box::new(Intake::new(16, 4096)));
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let t0 = Instant::now();
    let tick = |d: Duration| d.as_nanos().div_ceil(1_000) as u64;
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let sched = {
        let pool = pool.clone();
        std::thread::spawn(move || {
            let sleeper = Sleeper::new(hires);
            sleeper.register(intake);
            let _ = ready_tx.send(());
            let mut wheel = crate::proto_wheel::Wheel::new(n);
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                intake.drain(|d, p| {
                    wheel.insert(d, p).expect("capacity is n");
                });
                let now = (t0.elapsed().as_nanos() / 1_000) as u64;
                wheel.advance(now, |_, _, i| {
                    let i = i as usize;
                    if !cancelled[i].load(Ordering::Relaxed) {
                        let due = t0 + ds[i];
                        pool.spawn(move || {
                            out.record(i, due, cancelled[i].load(Ordering::Relaxed))
                        });
                    }
                });
                let next = wheel.next_deadline();
                if !intake.sleep_toward(next) {
                    continue; // a submit slipped in
                }
                match next {
                    None => sleeper.sleep(None),
                    Some(d) => {
                        let target = t0 + Duration::from_micros(d);
                        let now = Instant::now();
                        if target > now {
                            sleeper.sleep(Some(target - now));
                        }
                    }
                }
            }
        })
    };
    let _ = ready_rx.recv();
    let ts = Instant::now();
    for (i, d) in ds.iter().enumerate() {
        intake.submit(tick(*d), i as u64);
    }
    let submit = ts.elapsed();
    let tc = Instant::now();
    for c in cancelled.iter().step_by(2) {
        c.store(true, Ordering::Relaxed);
    }
    let cancel = tc.elapsed();
    let expected = (n - n.div_ceil(2)) as u64;
    wait_fired(out, expected, window + Duration::from_secs(30));
    stop.store(true, Ordering::Relaxed);
    intake.wake_timer();
    let _ = sched.join();
    if let Ok(p) = Arc::try_unwrap(pool) {
        p.shutdown();
    }
    out.finish(submit, cancel, n, n.div_ceil(2))
}

// ---------------------------------------------------------------------------
// proto-wheel-handle (experiment I-44)
// ---------------------------------------------------------------------------

/// `proto-wheel-handle`: `proto-wheel-intake` with a real cancel. Each submit
/// takes a slot (`proto_cancel::Slots`) and returns a handle that pins it;
/// cancel is one release store into the slot, which the timer checks before
/// handing the job to a worker and the worker checks again before running.
/// The slot is freed when both the handle and the timer side let go, so a
/// cancel can never land on a reused slot. Handle drop (one RMW) is timed
/// separately.
pub fn d01_wheel_handle(workers: usize, n: usize, window: Duration) -> DelayResult {
    use crate::proto_cancel::{Handle, Slots};
    use crate::proto_intake::Intake;
    let pool = Arc::new(CrossbeamPool::start(workers));
    let out = Outcome::new(n);
    let ds: &'static [Duration] = Box::leak(delays(n, window).into_boxed_slice());
    let intake: &'static Intake = Box::leak(Box::new(Intake::new(16, 4096)));
    let slots: &'static Slots = Box::leak(Box::new(Slots::new(n)));
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let t0 = Instant::now();
    let tick = |d: Duration| d.as_nanos().div_ceil(1_000) as u64;
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let sched = {
        let pool = pool.clone();
        std::thread::spawn(move || {
            intake.set_timer(std::thread::current());
            let _ = ready_tx.send(());
            let mut wheel = crate::proto_wheel::Wheel::new(n);
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                intake.drain(|d, p| {
                    wheel.insert(d, p).expect("capacity is n");
                });
                let now = (t0.elapsed().as_nanos() / 1_000) as u64;
                wheel.advance(now, |_, _, p| {
                    let slot = (p >> 32) as u32;
                    let i = (p & 0xffff_ffff) as usize;
                    if slots.is_cancelled(slot) {
                        slots.release(slot);
                    } else {
                        let due = t0 + ds[i];
                        // The timer's pin moves to the job.
                        pool.spawn(move || {
                            out.record(i, due, slots.is_cancelled(slot));
                            slots.release(slot);
                        });
                    }
                });
                let next = wheel.next_deadline();
                if !intake.sleep_toward(next) {
                    continue;
                }
                match next {
                    None => std::thread::park(),
                    Some(d) => {
                        let target = t0 + Duration::from_micros(d);
                        let now = Instant::now();
                        if target > now {
                            std::thread::park_timeout(target - now);
                        }
                    }
                }
            }
        })
    };
    let _ = ready_rx.recv();
    let mut handles = Vec::with_capacity(n);
    let ts = Instant::now();
    for (i, d) in ds.iter().enumerate() {
        let slot = slots.alloc().expect("capacity is n");
        intake.submit(tick(*d), (u64::from(slot) << 32) | i as u64);
        handles.push(Handle::new(slots, slot));
    }
    let submit = ts.elapsed();
    let tc = Instant::now();
    for h in handles.iter().step_by(2) {
        h.cancel();
    }
    let cancel = tc.elapsed();
    let td = Instant::now();
    drop(handles);
    let dropped = td.elapsed();
    let expected = (n - n.div_ceil(2)) as u64;
    wait_fired(out, expected, window + Duration::from_secs(30));
    stop.store(true, Ordering::Relaxed);
    intake.wake_timer();
    let _ = sched.join();
    if let Ok(p) = Arc::try_unwrap(pool) {
        p.shutdown();
    }
    let mut r = out.finish(submit, cancel, n, n.div_ceil(2));
    r.ns_per_drop = Some(dropped.as_nanos() as f64 / n as f64);
    r
}

// ---------------------------------------------------------------------------
// I-43 submit scaling
// ---------------------------------------------------------------------------

/// Experiment I-43, the submit path alone: one thread submits `n` delayed
/// jobs to an intake with room for all of them and no timer thread, with
/// and without the SeqCst fence of the wake protocol. Returns ns per submit
/// (fenced, unfenced).
pub fn submit_fence_cost(n: usize) -> (f64, f64) {
    use crate::proto_intake::Intake;
    let run = |fenced: bool| {
        let q = Intake::new(16, n.div_ceil(16));
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let t = Instant::now();
        for k in 0..n {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let d = 3_600_000_000 + x % 10_000_000;
            if fenced {
                q.submit(d, k as u64);
            } else {
                q.submit_without_fence(d, k as u64);
            }
        }
        t.elapsed().as_nanos() as f64 / n as f64
    };
    (run(true), run(false))
}

/// Experiment I-43, many submitters: `submitters` threads each submit their
/// share of `n` delayed jobs due an hour out (so nothing fires and only the
/// submit path is measured; deadlines spread over 10 s, in a different
/// order per submitter), to heap-scheduler's structure (one mutex and a
/// condvar, notified when a deadline is the new earliest) or to the intake.
/// `intake` is `Some(entries per ring)` (16 rings) or `None` for the heap.
/// A timer thread runs in both, waiting as it would. Returns ns per submit
/// over the whole batch (wall time / n).
pub fn submit_scaling(intake: Option<usize>, submitters: usize, n: usize) -> f64 {
    use crate::proto_intake::Intake;
    let per = n / submitters;
    let far = 3_600_000_000u64; // µs
    let go = Arc::new(std::sync::Barrier::new(submitters + 1));
    let stop = Arc::new(AtomicBool::new(false));
    let elapsed;
    if let Some(per_ring) = intake {
        let q: &'static Intake = Box::leak(Box::new(Intake::new(16, per_ring)));
        let timer = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                q.set_timer(std::thread::current());
                let mut wheel = crate::proto_wheel::Wheel::new(n);
                while !stop.load(Ordering::Relaxed) {
                    q.drain(|d, p| {
                        wheel.insert(d, p).expect("capacity is n");
                    });
                    let next = wheel.next_deadline();
                    if q.sleep_toward(next) {
                        std::thread::park_timeout(Duration::from_millis(100));
                    }
                }
            })
        };
        let hs: Vec<_> = (0..submitters)
            .map(|s| {
                let go = go.clone();
                std::thread::spawn(move || {
                    go.wait();
                    let mut x = 0x2545_F491_4F6C_DD1Du64 ^ s as u64;
                    for k in 0..per {
                        x ^= x << 13;
                        x ^= x >> 7;
                        x ^= x << 17;
                        q.submit(far + x % 10_000_000, k as u64);
                    }
                })
            })
            .collect();
        go.wait();
        let t = Instant::now();
        for h in hs {
            h.join().unwrap();
        }
        elapsed = t.elapsed();
        stop.store(true, Ordering::Relaxed);
        q.wake_timer();
        let _ = timer.join();
    } else {
        let heap: &'static Heap = Box::leak(Box::new(Heap {
            q: Mutex::new(BinaryHeap::with_capacity(n)),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
        }));
        let timer = std::thread::spawn(move || {
            let mut q = heap.q.lock().expect("heap");
            while !heap.stop.load(Ordering::Relaxed) {
                q = heap
                    .cv
                    .wait_timeout(q, Duration::from_millis(100))
                    .expect("heap")
                    .0;
            }
        });
        let base = Instant::now();
        let hs: Vec<_> = (0..submitters)
            .map(|s| {
                let go = go.clone();
                std::thread::spawn(move || {
                    go.wait();
                    let mut x = 0x2545_F491_4F6C_DD1Du64 ^ s as u64;
                    for k in 0..per {
                        x ^= x << 13;
                        x ^= x >> 7;
                        x ^= x << 17;
                        let due = base + Duration::from_micros(far + x % 10_000_000);
                        let mut q = heap.q.lock().expect("heap");
                        let earliest = q.peek().is_none_or(|Reverse((e, _))| due < *e);
                        q.push(Reverse((due, k)));
                        drop(q);
                        if earliest {
                            heap.cv.notify_one();
                        }
                    }
                })
            })
            .collect();
        go.wait();
        let t = Instant::now();
        for h in hs {
            h.join().unwrap();
        }
        elapsed = t.elapsed();
        heap.stop.store(true, Ordering::Relaxed);
        heap.cv.notify_all();
        let _ = timer.join();
    }
    elapsed.as_nanos() as f64 / (per * submitters) as f64
}
