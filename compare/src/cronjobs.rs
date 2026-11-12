//! Suite D-02 (dev/BENCHMARKS.md section 4.2): many cron schedules firing on
//! the same boundary.
//!
//! Contenders:
//! - `tokio-cron`: tokio-cron-scheduler, one `Job::new_async` per schedule on
//!   one `JobScheduler` (its README and docs).
//! - `apalis-cron`: one apalis worker per schedule with a `CronScheduler`
//!   backend (apalis-cron README, "Using `cron` crate").
//! - `proto-cron`: an experiment, not a competitor (`d02_proto`).
//!
//! Lateness is measured from the scheduled boundary: apalis gives the tick's
//! timestamp; for tokio-cron-scheduler the boundary is the start of the
//! current second or minute, which is exact as long as a job fires less than
//! one period late (anything later is reported as a missed period).

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct CronResult {
    pub lateness: hdrhistogram::Histogram<u64>,
    pub fired: u64,
    pub expected: u64,
    pub cpu_ms: Option<f64>,
    /// How many times the timer thread woke (`proto-cron` only).
    pub wakeups: Option<u64>,
    /// Ticks whose job ran before their wall-clock time (`proto-cron` only;
    /// the competitors' lateness is measured from the boundary, which cannot
    /// show it).
    pub early: Option<u64>,
}

#[derive(Clone, Copy)]
pub enum Period {
    Second,
    Minute,
}

impl Period {
    fn secs(self) -> u64 {
        match self {
            Period::Second => 1,
            Period::Minute => 60,
        }
    }
    /// Six-field expression (seconds first).
    fn expr(self) -> &'static str {
        match self {
            Period::Second => "* * * * * *",
            Period::Minute => "0 * * * * *",
        }
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

struct Lateness {
    samples: std::sync::Mutex<Vec<u64>>,
    fired: AtomicU64,
}

impl Lateness {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            samples: std::sync::Mutex::new(Vec::new()),
            fired: AtomicU64::new(0),
        })
    }
    fn record_boundary(&self, period: Period) {
        let now = now_ns();
        let p = period.secs() * 1_000_000_000;
        self.record(now % p);
    }
    fn record(&self, late_ns: u64) {
        self.fired.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut s) = self.samples.lock() {
            s.push(late_ns.max(1));
        }
    }
    fn finish(&self, expected: u64, cpu_ms: Option<f64>) -> CronResult {
        let mut h = crate::stats::histogram();
        if let Ok(s) = self.samples.lock() {
            for v in s.iter() {
                let _ = h.record(*v);
            }
        }
        CronResult {
            lateness: h,
            fired: self.fired.load(Ordering::Relaxed),
            expected,
            cpu_ms,
            wakeups: None,
            early: None,
        }
    }
}

fn cpu() -> Option<std::time::Duration> {
    crate::sys::process_cpu_time()
}

fn cpu_ms(a: Option<Duration>, b: Option<Duration>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(b.saturating_sub(a).as_secs_f64() * 1000.0),
        _ => None,
    }
}

pub fn d02_tokio(schedules: usize, period: Period, run_for: Duration) -> CronResult {
    use tokio_cron_scheduler::{Job, JobScheduler};
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let late = Lateness::new();
    let expected = schedules as u64 * (run_for.as_secs() / period.secs());
    rt.block_on(async {
        let sched = JobScheduler::new().await.expect("scheduler");
        for _ in 0..schedules {
            let late = late.clone();
            let job = Job::new_async(period.expr(), move |_id, _l| {
                let late = late.clone();
                Box::pin(async move { late.record_boundary(period) })
            })
            .expect("job");
            sched.add(job).await.expect("add");
        }
        let c0 = cpu();
        sched.start().await.expect("start");
        tokio::time::sleep(run_for).await;
        let c1 = cpu();
        let mut sched = sched;
        let _ = sched.shutdown().await;
        late.finish(expected, cpu_ms(c0, c1))
    })
}

async fn apalis_tick(tick: apalis_cron::Tick, late: apalis::prelude::Data<Arc<Lateness>>) {
    let due = tick.get_timestamp().saturating_mul(1_000_000_000);
    late.record(now_ns().saturating_sub(due));
}

pub fn d02_apalis(schedules: usize, period: Period, run_for: Duration) -> CronResult {
    use apalis::prelude::*;
    use apalis_cron::CronScheduler;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let late = Lateness::new();
    let expected = schedules as u64 * (run_for.as_secs() / period.secs());
    rt.block_on(async {
        let c0 = cpu();
        let mut handles = Vec::with_capacity(schedules);
        for k in 0..schedules {
            let schedule = cron::Schedule::from_str(period.expr()).expect("cron expr");
            let worker = WorkerBuilder::new(format!("cron-{k}"))
                .backend(CronScheduler::new(schedule))
                .data(late.clone())
                .build(apalis_tick);
            handles.push(tokio::spawn(async move {
                let _ = tokio::time::timeout(run_for, worker.run()).await;
            }));
        }
        for h in handles {
            let _ = h.await;
        }
        let c1 = cpu();
        late.finish(expected, cpu_ms(c0, c1))
    })
}

/// `proto-cron` (experiments I-17 and I-07): every schedule parsed once by
/// `proto_cron`, its next fire time kept in the `proto_wheel` timing wheel
/// (1 ms ticks) on one timer thread, which sleeps until the earliest one and
/// hands each due tick to crossbeam-pool (4 workers, as tokio-cron's
/// runtime has). Five-field cron has no seconds field, so this row runs only
/// in the full protocol (every minute). Also reports how often the timer
/// thread woke, to check I-17's claim that it wakes only when a schedule is
/// due.
pub fn d02_proto(schedules: usize, run_for: Duration) -> CronResult {
    use crate::contenders::{Contender, CrossbeamPool};
    use std::time::Instant;
    let cron = crate::proto_cron::Cron::parse("* * * * *").expect("every minute");
    let pool = Arc::new(CrossbeamPool::start(4));
    let late = Lateness::new();
    let early = Arc::new(AtomicU64::new(0));
    let expected = schedules as u64 * (run_for.as_secs() / 60);
    // Cron times are wall-clock times, so the wheel's ticks are wall-clock
    // milliseconds, read again at every wake: mapping them once onto the
    // monotonic clock let the two drift apart, and jobs fired before their
    // wall-clock minute (seen on WSL2 in the first run). A wall clock that
    // steps back only delays firing (the wheel never moves back); one that
    // steps forward fires what is due once, and the next fire time is
    // computed from now, so missed minutes do not fire in a burst.
    let unix_ms = || now_ns() / 1_000_000;
    let c0 = cpu();
    let stop_at = Instant::now() + run_for;
    let timer = {
        let pool = pool.clone();
        let late = late.clone();
        let early = early.clone();
        std::thread::spawn(move || {
            let mut wheel = crate::proto_wheel::Wheel::new(schedules);
            wheel.advance(unix_ms(), |_, _, _| {});
            let first = cron
                .next_after((unix_ms() / 1000) as i64)
                .expect("every minute matches");
            for s in 0..schedules {
                wheel
                    .insert(first as u64 * 1000, s as u64)
                    .expect("capacity is the schedule count");
            }
            let mut wakeups = 0u64;
            let mut due = Vec::new();
            loop {
                let now = unix_ms();
                wheel.advance(now, |_, d, s| due.push((d, s)));
                for (d, s) in due.drain(..) {
                    let boundary_ns = d * 1_000_000;
                    let late = late.clone();
                    let early = early.clone();
                    pool.spawn(move || {
                        let t = now_ns();
                        if t < boundary_ns {
                            early.fetch_add(1, Ordering::Relaxed);
                        }
                        late.record(t.saturating_sub(boundary_ns));
                    });
                    let after = (d.max(now) / 1000) as i64;
                    let next = cron.next_after(after).expect("every minute matches");
                    wheel
                        .insert(next as u64 * 1000, s)
                        .expect("a fired entry freed its place");
                }
                let t = Instant::now();
                if t >= stop_at {
                    return wakeups;
                }
                let left = stop_at - t;
                let sleep = match wheel.next_deadline() {
                    // Wall-clock distance, measured now; a wake before the
                    // wall-clock time just sleeps again.
                    Some(d) => Duration::from_millis(d.saturating_sub(unix_ms())).min(left),
                    None => left,
                };
                if !sleep.is_zero() {
                    std::thread::park_timeout(sleep);
                }
                wakeups += 1;
            }
        })
    };
    let wakeups = timer.join().expect("timer thread");
    let c1 = cpu();
    // Let the last boundary's jobs finish before counting.
    std::thread::sleep(Duration::from_millis(500));
    if let Ok(p) = Arc::try_unwrap(pool) {
        p.shutdown();
    }
    let mut r = late.finish(expected, cpu_ms(c0, c1));
    r.wakeups = Some(wakeups);
    r.early = Some(early.load(Ordering::Relaxed));
    r
}
