//! Suites W-06 and W-08 to W-13 (dev/BENCHMARKS.md section 4.1): lanes,
//! overload, panic storms, stuck handlers, shutdown, load steps, and faults.

use std::cell::RefCell;
use std::hint::spin_loop;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::contenders::{Contender, CrossbeamPool};
use crate::stats;
use crate::workers::{block_workers, open, spin_for, warm};

// ---------------------------------------------------------------------------
// Shared helpers.
// ---------------------------------------------------------------------------

#[repr(align(128))]
struct Padded(AtomicU64);

/// 64 padded counters; job `i` bumps counter `i % 64`.
struct Striped(Box<[Padded]>);

impl Striped {
    fn new() -> &'static Self {
        Box::leak(Box::new(Self(
            (0..64).map(|_| Padded(AtomicU64::new(0))).collect(),
        )))
    }
    #[inline]
    fn bump(&self, i: usize) {
        self.0[i % 64].0.fetch_add(1, Ordering::Relaxed);
    }
    fn sum(&self) -> u64 {
        self.0.iter().map(|p| p.0.load(Ordering::Relaxed)).sum()
    }
}

fn leak_slots(n: usize) -> &'static [AtomicU64] {
    Box::leak((0..n).map(|_| AtomicU64::new(0)).collect::<Box<[_]>>())
}

fn histogram_of(slots: &[AtomicU64]) -> hdrhistogram::Histogram<u64> {
    let mut h = stats::histogram();
    for s in slots {
        let v = s.load(Ordering::Relaxed);
        if v > 0 {
            let _ = h.record(v);
        }
    }
    h
}

/// Waits until `done` reaches `target`, polling every 5 ms.
fn wait_for(done: &Striped, target: u64) {
    while done.sum() < target {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Paces a submit loop: returns once the `i`-th submission at `rate` per
/// second (measured from `t0`) is due.
#[inline]
fn pace(t0: Instant, i: usize, rate: f64) {
    let due = t0 + Duration::from_secs_f64(i as f64 / rate);
    while Instant::now() < due {
        spin_loop();
    }
}

/// Samples the process's resident memory every 50 ms until stopped; returns
/// the peak.
struct RssMonitor {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<u64>,
}

impl RssMonitor {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let handle = std::thread::spawn(move || {
            let mut peak = 0;
            while !s.load(Ordering::Relaxed) {
                peak = peak.max(crate::sys::resident_bytes().unwrap_or(0));
                std::thread::sleep(Duration::from_millis(50));
            }
            peak
        });
        Self { stop, handle }
    }
    fn finish(self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().unwrap_or(0)
    }
}

/// Counts the workers that can still run a job: spawns `workers` jobs that
/// each check in and wait, and counts check-ins within `within`.
fn live_workers<C: Contender>(pool: &C, workers: usize, within: Duration) -> usize {
    let arrived: &'static AtomicUsize = Box::leak(Box::new(AtomicUsize::new(0)));
    let release: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    for _ in 0..workers {
        pool.spawn(move || {
            arrived.fetch_add(1, Ordering::AcqRel);
            let t = Instant::now();
            while !release.load(Ordering::Acquire) && t.elapsed() < Duration::from_secs(30) {
                std::thread::sleep(Duration::from_millis(1));
            }
        });
    }
    let t = Instant::now();
    while arrived.load(Ordering::Acquire) < workers && t.elapsed() < within {
        std::thread::sleep(Duration::from_millis(1));
    }
    let n = arrived.load(Ordering::Acquire);
    release.store(true, Ordering::Release);
    n
}

// ---------------------------------------------------------------------------
// W-06: weighted lanes. The only competitor is a hand-rolled pool with one
// bounded crossbeam channel per lane and a weighted pick, since none of the
// pools has lanes.
// ---------------------------------------------------------------------------

type Thunk = Box<dyn FnOnce() + Send + 'static>;

const WEIGHTS: [u64; 3] = [4, 2, 1];
const PATTERN: [usize; 7] = [0, 0, 0, 0, 1, 1, 2];

struct WeightedPool {
    txs: Vec<crossbeam_channel::Sender<Thunk>>,
    threads: Vec<JoinHandle<()>>,
}

impl WeightedPool {
    fn start(workers: usize) -> Self {
        let (txs, rxs): (Vec<_>, Vec<_>) = (0..3)
            .map(|_| crossbeam_channel::bounded::<Thunk>(1024))
            .unzip();
        let threads = (0..workers)
            .map(|w| {
                let rxs = rxs.clone();
                std::thread::spawn(move || {
                    let mut pos = w;
                    loop {
                        let pick = PATTERN[pos % PATTERN.len()];
                        pos += 1;
                        if let Ok(job) = rxs[pick].try_recv() {
                            job();
                            continue;
                        }
                        // The picked lane is empty: take from the highest
                        // non-empty lane instead.
                        if let Some(job) = rxs.iter().find_map(|rx| rx.try_recv().ok()) {
                            job();
                            continue;
                        }
                        // All empty: wait on all three.
                        let mut sel = crossbeam_channel::Select::new();
                        for rx in &rxs {
                            sel.recv(rx);
                        }
                        let op = sel.select();
                        let i = op.index();
                        match op.recv(&rxs[i]) {
                            Ok(job) => job(),
                            Err(_) => break,
                        }
                    }
                })
            })
            .collect();
        Self { txs, threads }
    }

    fn shutdown(self) {
        drop(self.txs);
        for t in self.threads {
            let _ = t.join();
        }
    }
}

/// Per-thread counters, registered on first use so the main thread can read
/// them without any shared counter on the job path.
struct ThreadStats {
    lane_done: [AtomicU64; 3],
    samples: Mutex<Vec<u64>>,
}

struct Registry {
    id: u64,
    threads: Mutex<Vec<Arc<ThreadStats>>>,
}

static NEXT_REGISTRY: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static MINE: RefCell<Option<(u64, Arc<ThreadStats>)>> = const { RefCell::new(None) };
}

impl Registry {
    fn new() -> &'static Self {
        Box::leak(Box::new(Self {
            id: NEXT_REGISTRY.fetch_add(1, Ordering::Relaxed),
            threads: Mutex::new(Vec::new()),
        }))
    }

    fn with<R>(&'static self, f: impl FnOnce(&ThreadStats) -> R) -> R {
        MINE.with(|m| {
            let mut m = m.borrow_mut();
            if m.as_ref().map(|x| x.0) != Some(self.id) {
                let s = Arc::new(ThreadStats {
                    lane_done: [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)],
                    samples: Mutex::new(Vec::new()),
                });
                self.threads.lock().expect("registry").push(s.clone());
                *m = Some((self.id, s));
            }
            f(&m.as_ref().expect("registered").1)
        })
    }

    fn lane_totals(&self) -> [u64; 3] {
        let mut t = [0; 3];
        for s in self.threads.lock().expect("registry").iter() {
            for (k, slot) in t.iter_mut().enumerate() {
                *slot += s.lane_done[k].load(Ordering::Relaxed);
            }
        }
        t
    }

    fn clear_samples(&self) {
        for s in self.threads.lock().expect("registry").iter() {
            s.samples.lock().expect("samples").clear();
        }
    }

    fn samples(&self) -> Vec<u64> {
        let mut v = Vec::new();
        for s in self.threads.lock().expect("registry").iter() {
            v.extend(s.samples.lock().expect("samples").iter().copied());
        }
        v
    }
}

pub struct LaneResult {
    pub max_share_error_pct: f64,
    pub shares_pct: [f64; 3],
    pub top_lane: hdrhistogram::Histogram<u64>,
}

pub fn w06(workers: usize, measure: Duration) -> LaneResult {
    const WORK: Duration = Duration::from_micros(10);
    let pool = WeightedPool::start(workers);
    let reg = Registry::new();
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let submitters: Vec<_> = (0..3)
        .map(|k| {
            let tx = pool.txs[k].clone();
            std::thread::spawn(move || {
                let mut n = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let t = Instant::now();
                    let sample = k == 0 && n.is_multiple_of(16);
                    let job: Thunk = Box::new(move || {
                        reg.with(|s| {
                            s.lane_done[k].fetch_add(1, Ordering::Relaxed);
                            if sample {
                                s.samples
                                    .lock()
                                    .expect("samples")
                                    .push(t.elapsed().as_nanos() as u64);
                            }
                        });
                        spin_for(WORK);
                    });
                    if tx.send(job).is_err() {
                        break;
                    }
                    n += 1;
                }
            })
        })
        .collect();
    // One second of warm-up, then the measured window.
    std::thread::sleep(Duration::from_secs(1));
    let before = reg.lane_totals();
    reg.clear_samples();
    std::thread::sleep(measure);
    let after = reg.lane_totals();
    let samples = reg.samples();
    stop.store(true, Ordering::Relaxed);
    for s in submitters {
        let _ = s.join();
    }
    pool.shutdown();
    let done: Vec<f64> = (0..3).map(|k| (after[k] - before[k]) as f64).collect();
    let total: f64 = done.iter().sum::<f64>().max(1.0);
    let wsum: u64 = WEIGHTS.iter().sum();
    let mut shares = [0.0; 3];
    let mut max_err: f64 = 0.0;
    for k in 0..3 {
        shares[k] = 100.0 * done[k] / total;
        let want = 100.0 * WEIGHTS[k] as f64 / wsum as f64;
        max_err = max_err.max((shares[k] - want).abs());
    }
    let mut h = stats::histogram();
    for v in samples {
        let _ = h.record(v.max(1));
    }
    LaneResult {
        max_share_error_pct: max_err,
        shares_pct: shares,
        top_lane: h,
    }
}

// ---------------------------------------------------------------------------
// W-08: overload at 2x capacity.
// ---------------------------------------------------------------------------

pub struct OverloadResult {
    pub admitted: hdrhistogram::Histogram<u64>,
    pub refused_pct: f64,
    pub peak_rss: u64,
}

/// `load` is the offered load as a multiple of capacity (2.0 for W-08, 0.5
/// for the unloaded reference).
pub fn w08<C: Contender>(workers: usize, dur: Duration, load: f64) -> OverloadResult {
    const WORK: Duration = Duration::from_micros(100);
    const EVERY: usize = 16;
    let pool = C::start(workers);
    warm(&pool, 10_000);
    let rate = load * workers as f64 / WORK.as_secs_f64();
    let total = (rate * dur.as_secs_f64()) as usize;
    let slots = leak_slots(total / EVERY + 1);
    let done = Striped::new();
    let rss = RssMonitor::start();
    let mut admitted = 0u64;
    let t0 = Instant::now();
    for i in 0..total {
        pace(t0, i, rate);
        let t = Instant::now();
        let ok = pool.try_spawn(move || {
            if i % EVERY == 0 {
                slots[i / EVERY].store(t.elapsed().as_nanos() as u64 + 1, Ordering::Relaxed);
            }
            spin_for(WORK);
            done.bump(i);
        });
        admitted += u64::from(ok);
    }
    wait_for(done, admitted);
    let peak_rss = rss.finish();
    pool.shutdown();
    OverloadResult {
        admitted: histogram_of(slots),
        refused_pct: 100.0 * (total as u64 - admitted) as f64 / total.max(1) as f64,
        peak_rss,
    }
}

// ---------------------------------------------------------------------------
// W-09: panic storm.
// ---------------------------------------------------------------------------

pub struct PanicResult {
    pub jobs_per_s: f64,
    pub workers_lost: usize,
}

pub fn w09<C: Contender>(workers: usize, jobs: usize) -> PanicResult {
    const WORK: Duration = Duration::from_micros(1);
    let pool = C::start(workers);
    warm(&pool, 10_000);
    let done = Striped::new();
    let t0 = Instant::now();
    for i in 0..jobs {
        pool.spawn(move || {
            done.bump(i);
            if i % 10 == 0 {
                panic!("injected panic");
            }
            spin_for(WORK);
        });
    }
    wait_for(done, jobs as u64);
    let secs = t0.elapsed().as_secs_f64();
    // Give a contender that replaces dead threads a moment to do it.
    std::thread::sleep(Duration::from_millis(200));
    let live = live_workers(&pool, workers, Duration::from_secs(3));
    pool.shutdown();
    PanicResult {
        jobs_per_s: jobs as f64 / secs,
        workers_lost: workers.saturating_sub(live),
    }
}

// ---------------------------------------------------------------------------
// W-10: one handler blocks for 500 ms every second, at 85% load (at lower
// load the other workers absorb it and the suite would measure nothing).
// ---------------------------------------------------------------------------

pub fn w10<C: Contender>(workers: usize, dur: Duration) -> hdrhistogram::Histogram<u64> {
    const WORK: Duration = Duration::from_micros(100);
    const EVERY: usize = 8;
    let pool = C::start(workers);
    warm(&pool, 10_000);
    let rate = 0.85 * workers as f64 / WORK.as_secs_f64();
    let total = (rate * dur.as_secs_f64()) as usize;
    let slots = leak_slots(total / EVERY + 1);
    let done = Striped::new();
    let per_second = rate as usize;
    let mut stuck = 0u64;
    let t0 = Instant::now();
    for i in 0..total {
        pace(t0, i, rate);
        if per_second > 0 && i % per_second == 0 {
            stuck += 1;
            pool.spawn(|| std::thread::sleep(Duration::from_millis(500)));
        }
        let t = Instant::now();
        pool.spawn(move || {
            if i % EVERY == 0 {
                slots[i / EVERY].store(t.elapsed().as_nanos() as u64 + 1, Ordering::Relaxed);
            }
            spin_for(WORK);
            done.bump(i);
        });
    }
    let _ = stuck;
    wait_for(done, total as u64);
    pool.shutdown();
    histogram_of(slots)
}

// ---------------------------------------------------------------------------
// W-11: shutdown with a deadline and a full queue.
// ---------------------------------------------------------------------------

pub struct ShutdownResult {
    pub queued: u64,
    pub stop_ms: f64,
    pub threads_left: Option<i64>,
    pub completed: u64,
}

pub fn w11<C: Contender>(workers: usize, queued: usize) -> ShutdownResult {
    let baseline = crate::sys::thread_count();
    let pool = C::start(workers);
    let gate = block_workers(&pool, workers);
    let done = Striped::new();
    // Workers are held at a gate: a bounded contender fills up, so submit
    // without waiting and report how many were queued.
    let mut accepted = 0u64;
    for i in 0..queued {
        accepted += u64::from(pool.try_spawn(move || {
            spin_for(Duration::from_micros(1));
            done.bump(i);
        }));
    }
    open(gate);
    let t = Instant::now();
    pool.shutdown();
    let stop_ms = t.elapsed().as_secs_f64() * 1000.0;
    let completed = done.sum();
    std::thread::sleep(Duration::from_millis(200));
    let threads_left = match (baseline, crate::sys::thread_count()) {
        (Some(a), Some(b)) => Some(b as i64 - a as i64),
        _ => None,
    };
    ShutdownResult {
        queued: accepted,
        stop_ms,
        threads_left,
        completed,
    }
}

// ---------------------------------------------------------------------------
// W-12: load step. Arrivals jump to 10x, then drop back.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub enum Handler {
    /// 1 ms blocking sleep (IO-like).
    Blocking,
    /// 100 us of CPU work.
    Cpu,
}

impl Handler {
    pub fn base_rate(self) -> f64 {
        // 10% of 16 workers (the reference host's physical cores).
        match self {
            Handler::Blocking => 1_600.0,
            Handler::Cpu => 16_000.0,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Handler::Blocking => "1 ms blocking handlers",
            Handler::Cpu => "100 us CPU handlers",
        }
    }
}

pub struct StepPhases {
    pub base: Duration,
    pub step: Duration,
    pub after: Duration,
}

pub struct StepResult {
    pub step_p99_ns: u64,
    pub settle_s: f64,
    pub retire_s: Option<f64>,
    pub reversals: Option<usize>,
    pub peak_threads: Option<usize>,
}

pub fn w12<C: Contender>(workers: usize, h: Handler, ph: &StepPhases) -> StepResult {
    let pool = C::start(workers);
    warm(&pool, 1_000);
    let base = h.base_rate();
    let (b, s, a) = (
        ph.base.as_secs_f64(),
        ph.step.as_secs_f64(),
        ph.after.as_secs_f64(),
    );
    let total = (base * b + 10.0 * base * s + base * a) as usize;
    // Each sample packs the submit offset (ms, high 24 bits) and the latency
    // (ns, low 40 bits).
    let slots = leak_slots(total);
    let done = Striped::new();
    // Thread-count samples every 100 ms.
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let t0 = Instant::now();
            let mut v = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                if let Some(n) = crate::sys::thread_count() {
                    v.push((t0.elapsed().as_secs_f64(), n));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            v
        })
    };
    let t0 = Instant::now();
    let mut i = 0usize;
    let mut next = 0.0f64;
    while i < total {
        let el = t0.elapsed().as_secs_f64();
        let rate = if el >= b && el < b + s {
            10.0 * base
        } else {
            base
        };
        if el < next {
            spin_loop();
            continue;
        }
        next += 1.0 / rate;
        let t = Instant::now();
        let offset_ms = (el * 1000.0) as u64;
        let idx = i;
        pool.spawn(move || {
            let lat = (t.elapsed().as_nanos() as u64).min((1 << 40) - 1);
            slots[idx].store((offset_ms << 40) | lat | 1, Ordering::Relaxed);
            match h {
                Handler::Blocking => std::thread::sleep(Duration::from_millis(1)),
                Handler::Cpu => spin_for(Duration::from_micros(100)),
            }
            done.bump(idx);
        });
        i += 1;
    }
    wait_for(done, total as u64);
    // Watch the thread count for the tail phase too.
    std::thread::sleep(ph.after);
    stop.store(true, Ordering::Relaxed);
    let threads = sampler.join().unwrap_or_default();
    pool.shutdown();

    // Latency during the step, and per-second p99 inside it.
    let mut step = stats::histogram();
    let windows = s.ceil() as usize;
    let mut per_window: Vec<hdrhistogram::Histogram<u64>> =
        (0..windows).map(|_| stats::histogram()).collect();
    for slot in slots {
        let v = slot.load(Ordering::Relaxed);
        if v == 0 {
            continue;
        }
        let off = (v >> 40) as f64 / 1000.0;
        let lat = (v & ((1 << 40) - 1)).max(1);
        if off >= b && off < b + s {
            let _ = step.record(lat);
            let w = ((off - b) as usize).min(windows - 1);
            let _ = per_window[w].record(lat);
        }
    }
    let step_p99 = step.value_at_quantile(0.99);
    // Settled: from the first window after which every window's p99 stays
    // within 1.2x of the p99 over the last third of the step, with 1 ms of
    // absolute tolerance so sub-millisecond noise does not count as unsettled.
    let tail_from = windows * 2 / 3;
    let mut tail = stats::histogram();
    for w in &per_window[tail_from..] {
        let _ = tail.add(w);
    }
    let tail_p99 = tail.value_at_quantile(0.99);
    let target = ((tail_p99 as f64 * 1.2) as u64).max(tail_p99 + 1_000_000);
    let mut settle = windows;
    for w in (0..windows).rev() {
        if !per_window[w].is_empty() && per_window[w].value_at_quantile(0.99) > target {
            break;
        }
        settle = w;
    }
    // Thread-count analysis (where the OS exposes it).
    let (retire_s, reversals, peak) = if threads.is_empty() {
        (None, None, None)
    } else {
        let before: Vec<usize> = threads
            .iter()
            .filter(|(t, _)| *t < b)
            .map(|x| x.1)
            .collect();
        let baseline = before.iter().copied().max().unwrap_or(0);
        let peak = threads.iter().map(|x| x.1).max().unwrap_or(0);
        let retire = threads
            .iter()
            .filter(|(t, _)| *t >= b + s)
            .find(|(_, n)| *n <= baseline + 2)
            .map(|(t, _)| t - (b + s));
        let mut rev = 0;
        let mut dir = 0i32;
        for pair in threads.windows(2) {
            let d = (pair[1].1 as i64 - pair[0].1 as i64).signum() as i32;
            if d != 0 {
                if dir != 0 && d != dir {
                    rev += 1;
                }
                dir = d;
            }
        }
        (retire, Some(rev), Some(peak))
    };
    StepResult {
        step_p99_ns: step_p99,
        settle_s: settle as f64,
        retire_s,
        reversals,
        peak_threads: peak,
    }
}

/// Grid search for the best fixed crossbeam-pool size on W-12 (the bar an
/// elastic pool must beat). Returns (size, result) for the best p99.
pub fn w12_best_fixed(h: Handler, ph: &StepPhases) -> (usize, StepResult) {
    let sizes: &[usize] = match h {
        Handler::Blocking => &[16, 24, 32, 48, 64, 96, 128],
        Handler::Cpu => &[8, 12, 16, 24, 32],
    };
    let mut best: Option<(usize, StepResult)> = None;
    for &n in sizes {
        let r = w12::<CrossbeamPool>(n, h, ph);
        eprintln!(
            "    best-fixed candidate {n:>3}: step p99 {} ns",
            r.step_p99_ns
        );
        if best
            .as_ref()
            .is_none_or(|(_, b)| r.step_p99_ns < b.step_p99_ns)
        {
            best = Some((n, r));
        }
    }
    best.expect("grid is not empty")
}

// ---------------------------------------------------------------------------
// W-13: a handler that hangs. Steady 85% load of 1 ms jobs on `workers`; after
// 2 s one job hangs for 15 s. Measures the other jobs over the next 10 s.
// ---------------------------------------------------------------------------

pub struct FaultResult {
    pub p99_ns: u64,
    pub late_jobs: u64,
}

pub fn w13_hang<C: Contender>(workers: usize) -> FaultResult {
    const WORK: Duration = Duration::from_millis(1);
    const DEADLINE_NS: u64 = 100_000_000; // 100 ms
    let pool = C::start(workers);
    warm(&pool, 1_000);
    // 85% load: with one worker hung, the rest are overloaded, so a pool
    // that cannot replace it falls behind.
    let rate = 0.85 * workers as f64 / WORK.as_secs_f64();
    let warmup = (rate * 2.0) as usize;
    let measured = (rate * 10.0) as usize;
    let slots = leak_slots(measured);
    let done = Striped::new();
    let t0 = Instant::now();
    for i in 0..warmup + measured {
        pace(t0, i, rate);
        if i == warmup {
            pool.spawn(|| std::thread::sleep(Duration::from_secs(15)));
        }
        let t = Instant::now();
        pool.spawn(move || {
            if i >= warmup {
                slots[i - warmup].store(t.elapsed().as_nanos() as u64 + 1, Ordering::Relaxed);
            }
            spin_for(WORK);
            done.bump(i);
        });
    }
    wait_for(done, (warmup + measured) as u64);
    let h = histogram_of(slots);
    let late = slots
        .iter()
        .filter(|s| s.load(Ordering::Relaxed) > DEADLINE_NS)
        .count() as u64;
    // The hung job keeps one thread for up to 15 s; shutdown waits for it.
    pool.shutdown();
    FaultResult {
        p99_ns: h.value_at_quantile(0.99),
        late_jobs: late,
    }
}
