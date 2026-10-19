//! fang contender for task-io's benchmarks (P-01, R-01), on SQLite.
//!
//! ```text
//! cargo run --release -- <P-01|R-01|all> [--quick] [--host R1]
//! ```
//!
//! fang (asynk, SQLite) is configured at its documented best for these
//! workloads: a short worker sleep so polling does not throttle it, and the
//! smallest backoff its API allows (whole seconds). Results go to the same
//! `compare/results/` files as the main harness.

// Shared with the main harness; this binary uses only part of them.
#[allow(dead_code)]
#[path = "../../src/report.rs"]
mod report;
#[allow(dead_code)]
#[path = "../../src/stats.rs"]
mod stats;
#[allow(dead_code)]
#[path = "../../src/sys.rs"]
mod sys;

use std::collections::HashSet;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use fang::asynk::async_queue::{AsyncQueue, AsyncQueueable};
use fang::asynk::async_worker_pool::AsyncWorkerPool;
use fang::{AsyncRunnable, FangError, SleepParams, async_trait, typetag};
use report::{Report, Row};
use serde::{Deserialize, Serialize};

static COMPLETED: AtomicU64 = AtomicU64::new(0);
/// Each run gets its own database file: on Windows the previous run's file
/// can still be open when the next run starts.
static RUN: AtomicU64 = AtomicU64::new(0);
static FAILED_ONCE: LazyLock<Mutex<HashSet<u64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

#[derive(Serialize, Deserialize)]
struct FangJob {
    i: u64,
    payload: Vec<u8>,
    /// R-01: fail the first attempt of every tenth job.
    fail_first: bool,
}

#[typetag::serde]
#[async_trait]
impl AsyncRunnable for FangJob {
    async fn run(&self, _queue: &dyn AsyncQueueable) -> Result<(), FangError> {
        if self.fail_first && self.i % 10 == 0 {
            let first = FAILED_ONCE
                .lock()
                .map(|mut s| s.insert(self.i))
                .unwrap_or(false);
            if first {
                return Err(FangError {
                    description: "temporary failure (injected)".into(),
                });
            }
        }
        COMPLETED.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn max_retries(&self) -> i32 {
        3
    }

    // fang's backoff is in whole seconds; 1 s is the smallest it allows.
    fn backoff(&self, _attempt: u32) -> u32 {
        1
    }
}

/// A fresh SQLite database with fang's own schema (its
/// `sqlite_migrations/.../up.sql`), applied through sqlx so no second SQLite
/// library is linked.
async fn fresh_queue(tag: &str, pool_size: u32) -> AsyncQueue {
    let dir = std::env::temp_dir().join("task-io-compare-fang");
    let _ = std::fs::create_dir_all(&dir);
    let run = RUN.fetch_add(1, Ordering::Relaxed);
    let file = format!("fang-{tag}-{}-{run}.db", std::process::id());
    let path = dir.join(&file);
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
    }
    // fang parses this as a URL, where `sqlite://C:/...` would read `C:` as a
    // host; a relative URL from inside the directory avoids the drive letter.
    std::env::set_current_dir(&dir).expect("enter db dir");
    let url = format!("sqlite:{file}?mode=rwc");
    let opts = sqlx::sqlite::SqliteConnectOptions::from_str(&url)
        .expect("sqlite url")
        .pragma("journal_mode", "WAL")
        .pragma("synchronous", "FULL");
    let pool = sqlx::SqlitePool::connect_with(opts)
        .await
        .expect("sqlite connect");
    for stmt in include_str!("fang_schema.sql").split(';') {
        if !stmt.trim().is_empty() {
            sqlx::query(stmt).execute(&pool).await.expect("fang schema");
        }
    }
    pool.close().await;
    let mut q = AsyncQueue::builder()
        .uri(url)
        .max_pool_size(pool_size)
        .build();
    q.connect().await.expect("fang connect");
    q
}

fn p01(quick: bool, r: &mut Report) {
    let (target, time_box, runs) = if quick {
        (5_000u64, Duration::from_secs(10), 1)
    } else {
        (1_000_000u64, Duration::from_secs(120), 3)
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .expect("rt");
    for subs in [1usize, 32] {
        let mut rates = Vec::new();
        let mut hs = Vec::new();
        let mut measured = u64::MAX;
        for _ in 0..runs {
            let (rate, h, n) = rt.block_on(async {
                let q = fresh_queue("p01", subs as u32).await;
                let next = std::sync::Arc::new(AtomicU64::new(0));
                let t0 = Instant::now();
                let mut tasks = Vec::new();
                for _ in 0..subs {
                    let (q, next) = (q.clone(), next.clone());
                    tasks.push(tokio::spawn(async move {
                        let mut lat = Vec::new();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            if i >= target || t0.elapsed() >= time_box {
                                break;
                            }
                            let job = FangJob {
                                i,
                                payload: vec![(i & 0xff) as u8; 64],
                                fail_first: false,
                            };
                            let t = Instant::now();
                            if q.insert_task(&job).await.is_err() {
                                break;
                            }
                            lat.push(t.elapsed().as_nanos() as u64);
                        }
                        lat
                    }));
                }
                let mut h = stats::histogram();
                let mut n = 0u64;
                for t in tasks {
                    if let Ok(lat) = t.await {
                        n += lat.len() as u64;
                        for v in lat {
                            let _ = h.record(v.max(1));
                        }
                    }
                }
                (n as f64 / t0.elapsed().as_secs_f64(), h, n)
            });
            rates.push(rate);
            hs.push(h);
            measured = measured.min(n);
        }
        let v = format!(
            "durable submits, 64-byte payload, {subs} submitter(s), up to {target} or {} s (measured {measured})",
            time_box.as_secs()
        );
        r.push(Row::summary(
            "P-01",
            v.clone(),
            "fang",
            subs,
            "submits per second",
            "/s",
            true,
            &rates,
        ));
        let mut row = Row::latency("P-01", v, "fang", subs, &hs);
        row.metric = "submit-to-durable latency".into();
        r.push(row);
    }
}

fn r01(quick: bool, r: &mut Report) {
    let (n, runs) = if quick {
        (2_000u64, 1)
    } else {
        (100_000u64, 3)
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .expect("rt");
    let mut rates = Vec::new();
    let mut finished_all = true;
    for _ in 0..runs {
        COMPLETED.store(0, Ordering::Relaxed);
        if let Ok(mut s) = FAILED_ONCE.lock() {
            s.clear();
        }
        let (rate, finished) = rt.block_on(async {
            let q = fresh_queue("r01", 32).await;
            for i in 0..n {
                let job = FangJob {
                    i,
                    payload: vec![(i & 0xff) as u8; 64],
                    fail_first: true,
                };
                q.insert_task(&job).await.expect("insert");
            }
            let mut pool: AsyncWorkerPool<AsyncQueue> = AsyncWorkerPool::builder()
                .number_of_workers(32u32)
                .queue(q.clone())
                .sleep_params(SleepParams {
                    sleep_period: Duration::from_millis(10),
                    max_sleep_period: Duration::from_millis(100),
                    min_sleep_period: Duration::from_millis(10),
                    sleep_step: Duration::from_millis(10),
                })
                .build();
            let t0 = Instant::now();
            pool.start().await;
            let mut finished = false;
            while t0.elapsed() < Duration::from_secs(600) {
                if COMPLETED.load(Ordering::Relaxed) >= n {
                    finished = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            (
                COMPLETED.load(Ordering::Relaxed) as f64 / t0.elapsed().as_secs_f64(),
                finished,
            )
        });
        rates.push(rate);
        finished_all &= finished;
    }
    let v = format!("10% fail once, fang backoff 1 s (its minimum), {n} jobs");
    if finished_all {
        r.push(Row::summary(
            "R-01",
            v,
            "fang",
            32,
            "completed jobs per second",
            "/s",
            true,
            &rates,
        ));
    } else {
        r.push(Row::missing("R-01", v, "fang", 32, "did not finish within 10 minutes").info());
    }
}

fn main() {
    let mut quick = false;
    let mut host = "local".to_string();
    let mut suites = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--quick" => quick = true,
            "--host" => host = args.next().unwrap_or_default(),
            s => suites.push(s.to_string()),
        }
    }
    if suites.is_empty() || suites.iter().any(|s| s == "all") {
        suites = vec!["P-01".into(), "R-01".into()];
    }
    let mut report = Report::new(&host, quick);
    for s in &suites {
        eprintln!("== {s}");
        match s.as_str() {
            "P-01" => p01(quick, &mut report),
            "R-01" => r01(quick, &mut report),
            other => eprintln!("unknown suite {other}"),
        }
    }
    report.finish();
}
