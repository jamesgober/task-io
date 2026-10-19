//! Suites R-01, R-02 (dev/BENCHMARKS.md section 4.3) and K-01 (section 4.4)
//! against apalis on SQLite at full durability, with apalis's documented
//! retry layer (`RetryPolicy::retries(n).with_backoff(..)`, apalis
//! `layers::retry` docs) and idempotency keys (`TaskBuilder::idempotency_key`).
//!
//! apalis-sqlite is the only durable job system in this harness so far (fang
//! needs its own crate; background-jobs waits on PLAN O-9), so these rows set
//! a provisional bar.

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use apalis::layers::retry::RetryPolicy;
use apalis::prelude::*;
use apalis_sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteStorage};
use tower::retry::backoff::{ExponentialBackoffMaker, MakeBackoff};
use tower::util::rng::HasherRng;

use crate::durable::DurableJob;

/// Opens a fresh database at full durability (WAL, synchronous = FULL on
/// every connection), with apalis's schema.
async fn open_full(dir: &Path, tag: &str, conns: u32) -> SqlitePool {
    let path = dir.join(format!("apalis-{tag}.db"));
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
    }
    let url = format!(
        "sqlite://{}?mode=rwc",
        path.display().to_string().replace('\\', "/")
    );
    let base = SqliteConnectOptions::from_str(&url).expect("sqlite url");
    let setup = SqlitePool::connect_with(base.clone())
        .await
        .expect("sqlite connect");
    SqliteStorage::<()>::setup(&setup)
        .await
        .expect("apalis setup");
    setup.close().await;
    SqlitePoolOptions::new()
        .max_connections(conns)
        .connect_with(
            base.pragma("journal_mode", "WAL")
                .pragma("synchronous", "FULL"),
        )
        .await
        .expect("sqlite pool")
}

fn backoff(min: Duration, max: Duration) -> tower::retry::backoff::ExponentialBackoff<HasherRng> {
    ExponentialBackoffMaker::new(min, max, 1.0, HasherRng::default())
        .expect("backoff bounds")
        .make_backoff()
}

#[derive(Default)]
struct Counters {
    attempts: AtomicU64,
    completed: AtomicU64,
    dead: AtomicU64,
}

/// Pushes `n` jobs (time-boxed) and returns how many were stored.
async fn push_jobs(storage: &SqliteStorage<DurableJob>, n: u64, time_box: Duration) -> u64 {
    let mut s = storage.clone();
    let t0 = Instant::now();
    let mut pushed = 0;
    for i in 0..n {
        if t0.elapsed() >= time_box {
            break;
        }
        let job = DurableJob {
            i,
            payload: vec![(i & 0xff) as u8; 64],
        };
        if s.push(job).await.is_err() {
            break;
        }
        pushed += 1;
    }
    pushed
}

/// Waits until `done()` reaches `target` or `limit` passes.
async fn wait_until(limit: Duration, target: u64, done: impl Fn() -> u64) -> bool {
    let t0 = Instant::now();
    while done() < target {
        if t0.elapsed() > limit {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    true
}

// ---------------------------------------------------------------------------
// R-01: 10% of jobs fail on their first attempt and are retried with backoff.
// ---------------------------------------------------------------------------

pub struct RetryResult {
    pub jobs: u64,
    pub jobs_per_s: f64,
    pub finished: bool,
}

async fn r01_handler(
    job: DurableJob,
    attempt: Attempt,
    c: Data<Arc<Counters>>,
) -> Result<(), BoxDynError> {
    c.attempts.fetch_add(1, Ordering::Relaxed);
    if job.i.is_multiple_of(10) && attempt.current() == 0 {
        return Err("temporary failure (injected)".into());
    }
    c.completed.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

pub fn r01_apalis(dir: &Path, n: u64, time_box: Duration) -> RetryResult {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .expect("rt");
    rt.block_on(async {
        let pool = open_full(dir, "r01", 32).await;
        let storage = SqliteStorage::<DurableJob>::new(&pool);
        let jobs = push_jobs(&storage, n, time_box).await;
        let c = Arc::new(Counters::default());
        let worker = WorkerBuilder::new("r01")
            .backend(storage)
            .retry(
                RetryPolicy::retries(3)
                    .with_backoff(backoff(Duration::from_millis(10), Duration::from_secs(1))),
            )
            .concurrency(32)
            .data(c.clone())
            .build(r01_handler);
        let t0 = Instant::now();
        let handle = tokio::spawn(async move {
            let _ = worker.run().await;
        });
        let cc = c.clone();
        let finished = wait_until(Duration::from_secs(600), jobs, || {
            cc.completed.load(Ordering::Relaxed)
        })
        .await;
        let secs = t0.elapsed().as_secs_f64();
        handle.abort();
        pool.close().await;
        RetryResult {
            jobs,
            jobs_per_s: c.completed.load(Ordering::Relaxed) as f64 / secs,
            finished,
        }
    })
}

// ---------------------------------------------------------------------------
// R-02: the downstream fails every attempt for `outage`, then recovers.
// ---------------------------------------------------------------------------

pub struct OutageResult {
    pub jobs: u64,
    pub attempts_during_outage: u64,
    pub drain_s: Option<f64>,
    pub completed: u64,
    pub dead: u64,
}

struct Outage {
    c: Counters,
    until: Instant,
    max_attempts: usize,
    during: AtomicU64,
}

async fn r02_handler(
    _job: DurableJob,
    attempt: Attempt,
    o: Data<Arc<Outage>>,
) -> Result<(), BoxDynError> {
    o.c.attempts.fetch_add(1, Ordering::Relaxed);
    if Instant::now() < o.until {
        o.during.fetch_add(1, Ordering::Relaxed);
        if attempt.current() + 1 >= o.max_attempts {
            o.c.dead.fetch_add(1, Ordering::Relaxed);
        }
        return Err("downstream unavailable (injected)".into());
    }
    o.c.completed.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

pub fn r02_apalis(dir: &Path, n: u64, outage: Duration, time_box: Duration) -> OutageResult {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .expect("rt");
    rt.block_on(async {
        let pool = open_full(dir, "r02", 32).await;
        let storage = SqliteStorage::<DurableJob>::new(&pool);
        let jobs = push_jobs(&storage, n, time_box).await;
        let max_attempts = 10usize;
        let o = Arc::new(Outage {
            c: Counters::default(),
            until: Instant::now() + outage,
            max_attempts,
            during: AtomicU64::new(0),
        });
        let worker = WorkerBuilder::new("r02")
            .backend(storage)
            .retry(
                RetryPolicy::retries(max_attempts - 1)
                    .with_backoff(backoff(Duration::from_millis(100), Duration::from_secs(2))),
            )
            .concurrency(32)
            .data(o.clone())
            .build(r02_handler);
        let handle = tokio::spawn(async move {
            let _ = worker.run().await;
        });
        let oo = o.clone();
        let done = wait_until(outage + Duration::from_secs(600), jobs, || {
            oo.c.completed.load(Ordering::Relaxed) + oo.c.dead.load(Ordering::Relaxed)
        })
        .await;
        let drain_s = done.then(|| {
            Instant::now()
                .saturating_duration_since(o.until)
                .as_secs_f64()
        });
        handle.abort();
        pool.close().await;
        OutageResult {
            jobs,
            attempts_during_outage: o.during.load(Ordering::Relaxed),
            drain_s,
            completed: o.c.completed.load(Ordering::Relaxed),
            dead: o.c.dead.load(Ordering::Relaxed),
        }
    })
}

// ---------------------------------------------------------------------------
// K-01: half the submits reuse an idempotency key.
// ---------------------------------------------------------------------------

pub struct KeyResult {
    pub submits: u64,
    pub ns_per_submit_keyed: f64,
    pub ns_per_submit_plain: f64,
    pub duplicate_runs: u64,
}

async fn k01_handler(job: DurableJob, runs: Data<Arc<Mutex<HashMap<u64, u32>>>>) {
    if let Ok(mut m) = runs.lock() {
        *m.entry(job.i / 2).or_insert(0) += 1;
    }
}

pub fn k01_apalis(dir: &Path, submitters: usize, n: u64, time_box: Duration) -> KeyResult {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .expect("rt");
    rt.block_on(async {
        // Plain submits for the cost comparison.
        let pool = open_full(dir, "k01-plain", submitters as u32).await;
        let storage = SqliteStorage::<DurableJob>::new(&pool);
        let t = Instant::now();
        let plain = push_jobs(&storage, n, time_box).await;
        let plain_ns = t.elapsed().as_nanos() as f64 / plain.max(1) as f64;
        pool.close().await;

        // Keyed submits: job i uses key i / 2, so every key is submitted twice.
        let pool = open_full(dir, "k01-keyed", submitters as u32).await;
        let storage = SqliteStorage::<DurableJob>::new(&pool);
        let next = Arc::new(AtomicU64::new(0));
        let submitted = Arc::new(AtomicU64::new(0));
        let t0 = Instant::now();
        let mut tasks = Vec::new();
        for _ in 0..submitters {
            let mut s = storage.clone();
            let (next, submitted) = (next.clone(), submitted.clone());
            tasks.push(tokio::spawn(async move {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n || t0.elapsed() >= time_box {
                        break;
                    }
                    let task = TaskBuilder::new(DurableJob {
                        i,
                        payload: vec![0; 64],
                    })
                    .idempotency_key(format!("k{}", i / 2))
                    .build();
                    // A refused duplicate is the expected outcome for half of them.
                    let _ = s.push_task(task).await;
                    submitted.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
        for t in tasks {
            let _ = t.await;
        }
        let total = submitted.load(Ordering::Relaxed);
        let keyed_ns = t0.elapsed().as_nanos() as f64 * submitters as f64 / total.max(1) as f64;
        let runs = Arc::new(Mutex::new(HashMap::<u64, u32>::new()));
        let worker = WorkerBuilder::new("k01")
            .backend(storage)
            .concurrency(32)
            .data(runs.clone())
            .build(k01_handler);
        let handle = tokio::spawn(async move {
            let _ = worker.run().await;
        });
        // Every distinct key should run once; give the worker time to drain.
        let keys = total.div_ceil(2);
        let rr = runs.clone();
        let _ = wait_until(Duration::from_secs(300), keys, || {
            rr.lock().map(|m| m.len() as u64).unwrap_or(0)
        })
        .await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        handle.abort();
        pool.close().await;
        let duplicate_runs = runs
            .lock()
            .map(|m| m.values().map(|&c| u64::from(c.saturating_sub(1))).sum())
            .unwrap_or(0);
        KeyResult {
            submits: total,
            ns_per_submit_keyed: keyed_ns,
            ns_per_submit_plain: plain_ns,
            duplicate_runs,
        }
    })
}
