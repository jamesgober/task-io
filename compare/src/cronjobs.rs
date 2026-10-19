//! Suite D-02 (dev/BENCHMARKS.md section 4.2): many cron schedules firing on
//! the same boundary.
//!
//! Contenders:
//! - `tokio-cron`: tokio-cron-scheduler, one `Job::new_async` per schedule on
//!   one `JobScheduler` (its README and docs).
//! - `apalis-cron`: one apalis worker per schedule with a `CronScheduler`
//!   backend (apalis-cron README, "Using `cron` crate").
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
