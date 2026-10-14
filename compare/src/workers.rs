//! Suites W-01 to W-05 (dev/BENCHMARKS.md section 4.1), generic over the
//! contender.

use std::hint::{black_box, spin_loop};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Barrier, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::contenders::Contender;
use crate::stats;

// ---------------------------------------------------------------------------
// Completion tracking with almost no contention: job `i` counts down stripe
// `i % 64`, and only the last job of each stripe touches the shared counter.
// A single shared counter would add its own contention at 32 workers and hide
// real differences between contenders.
// ---------------------------------------------------------------------------

#[repr(align(128))]
struct Padded(AtomicUsize);

pub struct StripedLatch {
    stripes: Box<[Padded]>,
    stripes_left: AtomicUsize,
    done: Mutex<bool>,
    cv: Condvar,
}

const STRIPES: usize = 64;

impl StripedLatch {
    pub fn new(jobs: usize) -> &'static Self {
        let stripes: Box<[Padded]> = (0..STRIPES)
            .map(|s| {
                Padded(AtomicUsize::new(
                    jobs / STRIPES + usize::from(s < jobs % STRIPES),
                ))
            })
            .collect();
        let nonzero = stripes
            .iter()
            .filter(|p| p.0.load(Ordering::Relaxed) > 0)
            .count();
        // Leaked on purpose: jobs hold `&'static` references, so no reference
        // count is touched per job. A few kilobytes per run.
        Box::leak(Box::new(Self {
            stripes,
            stripes_left: AtomicUsize::new(nonzero),
            done: Mutex::new(nonzero == 0),
            cv: Condvar::new(),
        }))
    }

    #[inline]
    pub fn count_down(&self, i: usize) {
        if self.stripes[i % STRIPES].0.fetch_sub(1, Ordering::AcqRel) == 1
            && self.stripes_left.fetch_sub(1, Ordering::AcqRel) == 1
        {
            *self.done.lock().expect("latch") = true;
            self.cv.notify_all();
        }
    }

    pub fn wait(&self) {
        let mut d = self.done.lock().expect("latch");
        while !*d {
            d = self.cv.wait(d).expect("latch");
        }
    }
}

#[inline]
pub fn spin_for(d: Duration) {
    let t = Instant::now();
    while t.elapsed() < d {
        spin_loop();
    }
}

fn warm<C: Contender>(pool: &C, jobs: usize) {
    let latch = StripedLatch::new(jobs);
    for i in 0..jobs {
        pool.spawn(move || latch.count_down(i));
    }
    latch.wait();
}

// ---------------------------------------------------------------------------
// W-01: dispatch throughput.
// ---------------------------------------------------------------------------

/// Seconds to run `jobs` empty jobs with an 8-byte payload, submitted from
/// `submitters` threads.
pub fn w01<C: Contender>(workers: usize, submitters: usize, jobs: usize) -> f64 {
    let pool = C::start(workers);
    warm(&pool, 100_000);
    let latch = StripedLatch::new(jobs);
    let secs;
    if submitters <= 1 {
        let t0 = Instant::now();
        for i in 0..jobs {
            pool.spawn(move || {
                black_box(i as u64);
                latch.count_down(i);
            });
        }
        latch.wait();
        secs = t0.elapsed().as_secs_f64();
    } else {
        let barrier = Barrier::new(submitters + 1);
        let mut measured = 0.0;
        std::thread::scope(|s| {
            for k in 0..submitters {
                let (pool, barrier) = (&pool, &barrier);
                let lo = jobs * k / submitters;
                let hi = jobs * (k + 1) / submitters;
                s.spawn(move || {
                    barrier.wait();
                    for i in lo..hi {
                        pool.spawn(move || {
                            black_box(i as u64);
                            latch.count_down(i);
                        });
                    }
                });
            }
            barrier.wait();
            let t0 = Instant::now();
            latch.wait();
            measured = t0.elapsed().as_secs_f64();
        });
        secs = measured;
    }
    pool.shutdown();
    secs
}

// ---------------------------------------------------------------------------
// W-02: submit-to-start latency.
// ---------------------------------------------------------------------------

struct Probe {
    ns: AtomicU64,
    ready: AtomicBool,
}

/// Idle pool: one job at a time, with `gap` between jobs so workers park.
pub fn w02_idle<C: Contender>(
    workers: usize,
    samples: usize,
    gap: Duration,
) -> hdrhistogram::Histogram<u64> {
    let pool = C::start(workers);
    warm(&pool, 10_000);
    let probe: &'static Probe = Box::leak(Box::new(Probe {
        ns: AtomicU64::new(0),
        ready: AtomicBool::new(false),
    }));
    let mut h = stats::histogram();
    for _ in 0..samples {
        // A busy-wait, not a sleep: OS sleep granularity (about 0.5 ms on
        // Windows) would make a million samples take hours. Workers still park,
        // since there is no work during the gap.
        spin_for(gap);
        let t = Instant::now();
        pool.spawn(move || {
            probe
                .ns
                .store(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
            probe.ready.store(true, Ordering::Release);
        });
        while !probe.ready.load(Ordering::Acquire) {
            spin_loop();
        }
        probe.ready.store(false, Ordering::Relaxed);
        let _ = h.record(probe.ns.load(Ordering::Relaxed).max(1));
    }
    pool.shutdown();
    h
}

/// Busy pool: 10 us jobs submitted at 80% of the pool's capacity.
pub fn w02_busy<C: Contender>(workers: usize, samples: usize) -> hdrhistogram::Histogram<u64> {
    const WORK: Duration = Duration::from_micros(10);
    let pool = C::start(workers);
    warm(&pool, 10_000);
    let slots: &'static [AtomicU64] = Box::leak(
        (0..samples)
            .map(|_| AtomicU64::new(0))
            .collect::<Box<[_]>>(),
    );
    let latch = StripedLatch::new(samples);
    let interval = WORK.as_secs_f64() / (0.8 * workers as f64);
    let t0 = Instant::now();
    for (i, slot) in slots.iter().enumerate() {
        let due = t0 + Duration::from_secs_f64(interval * i as f64);
        while Instant::now() < due {
            spin_loop();
        }
        let t = Instant::now();
        pool.spawn(move || {
            slot.store(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
            spin_for(WORK);
            latch.count_down(i);
        });
    }
    latch.wait();
    pool.shutdown();
    let mut h = stats::histogram();
    for s in slots {
        let _ = h.record(s.load(Ordering::Relaxed).max(1));
    }
    h
}

// ---------------------------------------------------------------------------
// W-03: overhead share with real work.
// ---------------------------------------------------------------------------

/// Fraction of the ideal throughput reached with jobs of `work` busy time.
pub fn w03<C: Contender>(workers: usize, work: Duration) -> f64 {
    let pool = C::start(workers);
    warm(&pool, 10_000);
    // About one second of ideal run time.
    let jobs = ((workers as f64) / work.as_secs_f64()).round().max(1.0) as usize;
    let latch = StripedLatch::new(jobs);
    let t0 = Instant::now();
    for i in 0..jobs {
        pool.spawn(move || {
            spin_for(work);
            latch.count_down(i);
        });
    }
    latch.wait();
    let wall = t0.elapsed().as_secs_f64();
    pool.shutdown();
    (jobs as f64 * work.as_secs_f64()) / (workers as f64 * wall)
}

// ---------------------------------------------------------------------------
// W-04: memory per queued job.
// ---------------------------------------------------------------------------

struct Gate {
    open: Mutex<bool>,
    cv: Condvar,
    started: AtomicUsize,
}

/// Occupies every worker with a job that waits on a gate.
fn block_workers<C: Contender>(pool: &C, workers: usize) -> &'static Gate {
    let gate: &'static Gate = Box::leak(Box::new(Gate {
        open: Mutex::new(false),
        cv: Condvar::new(),
        started: AtomicUsize::new(0),
    }));
    for _ in 0..workers {
        pool.spawn(move || {
            gate.started.fetch_add(1, Ordering::AcqRel);
            let mut o = gate.open.lock().expect("gate");
            while !*o {
                o = gate.cv.wait(o).expect("gate");
            }
        });
    }
    while gate.started.load(Ordering::Acquire) < workers {
        std::thread::sleep(Duration::from_millis(1));
    }
    gate
}

fn open(gate: &Gate) {
    *gate.open.lock().expect("gate") = true;
    gate.cv.notify_all();
}

pub struct MemResult {
    pub heap_bytes_per_job: f64,
    pub allocs_per_submit: f64,
    pub rss_bytes_per_job: Option<f64>,
}

/// Memory held per queued job with a 64-byte payload, after a warm-up batch.
pub fn w04<C: Contender>(workers: usize, jobs: usize) -> MemResult {
    let pool = C::start(workers);
    let submit_batch = |pool: &C| {
        let latch = StripedLatch::new(jobs);
        for i in 0..jobs {
            let payload = [i as u8; 64];
            pool.spawn(move || {
                black_box(&payload);
                latch.count_down(i);
            });
        }
        latch
    };
    // Warm-up batch so queues have grown to steady state.
    let gate = block_workers(&pool, workers);
    let latch = submit_batch(&pool);
    open(gate);
    latch.wait();
    // Measured batch.
    let gate = block_workers(&pool, workers);
    let rss0 = crate::sys::resident_bytes();
    crate::alloc::start();
    let a0 = crate::alloc::snapshot();
    let latch = submit_batch(&pool);
    let a1 = crate::alloc::snapshot();
    crate::alloc::stop();
    let rss1 = crate::sys::resident_bytes();
    open(gate);
    latch.wait();
    pool.shutdown();
    MemResult {
        heap_bytes_per_job: (a1.live_bytes - a0.live_bytes) as f64 / jobs as f64,
        allocs_per_submit: (a1.allocs - a0.allocs) as f64 / jobs as f64,
        rss_bytes_per_job: match (rss0, rss1) {
            (Some(a), Some(b)) => Some((b as f64 - a as f64) / jobs as f64),
            _ => None,
        },
    }
}

// ---------------------------------------------------------------------------
// W-05: idle cost.
// ---------------------------------------------------------------------------

pub struct IdleResult {
    pub cpu_ms_per_s: Option<f64>,
    pub wakeups_per_s: Option<f64>,
}

pub fn w05<C: Contender>(workers: usize, idle: Duration) -> IdleResult {
    let pool = C::start(workers);
    warm(&pool, 10_000);
    // Let the pool settle into its idle state first.
    std::thread::sleep(Duration::from_millis(500));
    let cpu0 = crate::sys::process_cpu_time();
    let cs0 = crate::sys::context_switches();
    std::thread::sleep(idle);
    let cpu1 = crate::sys::process_cpu_time();
    let cs1 = crate::sys::context_switches();
    pool.shutdown();
    let s = idle.as_secs_f64();
    IdleResult {
        cpu_ms_per_s: match (cpu0, cpu1) {
            (Some(a), Some(b)) => Some((b.saturating_sub(a)).as_secs_f64() * 1000.0 / s),
            _ => None,
        },
        // The sleeping main thread itself switches once; subtract it.
        wakeups_per_s: match (cs0, cs1) {
            (Some(a), Some(b)) => Some((b.saturating_sub(a).saturating_sub(1)) as f64 / s),
            _ => None,
        },
    }
}
