//! Suite P-01 (dev/BENCHMARKS.md section 4.5): durable submits, against
//! apalis on SQLite (apalis-sqlite README, "Basic Worker").
//!
//! apalis-sqlite's `setup()` runs `PRAGMA synchronous = OFF` (on one pooled
//! connection only), so by default its submits are not on stable storage when
//! acknowledged. P-01 requires equal durability, so the gated row opens every
//! connection with `journal_mode = WAL` and `synchronous = FULL`. The default
//! is measured too, as a reference row labelled as not power-safe.
//!
//! SQLite cannot group-commit across connections, so it manages far fewer
//! durable submits per second than an in-memory queue. Each run is therefore
//! time-boxed: up to the target count or the time box, whichever comes first,
//! and the row reports how many submits it measured.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use apalis::prelude::*;
use apalis_sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteStorage};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DurableJob {
    pub i: u64,
    pub payload: Vec<u8>,
}

pub struct DurableResult {
    pub latency: hdrhistogram::Histogram<u64>,
    pub submits: u64,
    pub per_second: f64,
}

#[derive(Clone, Copy)]
pub enum Sync {
    /// `synchronous = FULL` on every connection: on stable storage when
    /// acknowledged (the gated configuration).
    Full,
    /// apalis-sqlite's own `setup()` defaults (not power-safe).
    ApalisDefault,
}

fn db_path(dir: &Path, tag: &str) -> PathBuf {
    let p = dir.join(format!("apalis-{tag}.db"));
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", p.display()));
    }
    p
}

/// Up to `target` durable submits of 64-byte payloads from `submitters`
/// concurrent tasks, stopping at `time_box`.
pub fn p01_apalis(
    dir: &Path,
    sync: Sync,
    submitters: usize,
    target: u64,
    time_box: Duration,
) -> DurableResult {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let path = db_path(dir, match sync {
        Sync::Full => "full",
        Sync::ApalisDefault => "default",
    });
    rt.block_on(async move {
        let url = format!("sqlite://{}?mode=rwc", path.display().to_string().replace('\\', "/"));
        let base = SqliteConnectOptions::from_str(&url).expect("sqlite url");
        // Create the schema with apalis's own setup.
        let setup_pool = SqlitePool::connect_with(base.clone()).await.expect("sqlite connect");
        SqliteStorage::<DurableJob>::setup(&setup_pool).await.expect("apalis setup");
        let pool = match sync {
            Sync::Full => {
                setup_pool.close().await;
                let opts = base.pragma("journal_mode", "WAL").pragma("synchronous", "FULL");
                SqlitePoolOptions::new()
                    .max_connections(submitters as u32)
                    .connect_with(opts)
                    .await
                    .expect("sqlite pool")
            }
            Sync::ApalisDefault => setup_pool,
        };
        let storage = SqliteStorage::<DurableJob>::new(&pool);
        let next = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let t0 = Instant::now();
        let mut tasks = Vec::new();
        for _ in 0..submitters {
            let mut s = storage.clone();
            let (next, stop) = (next.clone(), stop.clone());
            tasks.push(tokio::spawn(async move {
                let mut lat = Vec::new();
                loop {
                    if stop.load(Ordering::Relaxed) || t0.elapsed() >= time_box {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= target {
                        break;
                    }
                    let job = DurableJob { i, payload: vec![(i & 0xff) as u8; 64] };
                    let t = Instant::now();
                    if s.push(job).await.is_err() {
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                    lat.push(t.elapsed().as_nanos() as u64);
                }
                lat
            }));
        }
        let mut h = crate::stats::histogram();
        let mut n = 0u64;
        for t in tasks {
            if let Ok(lat) = t.await {
                n += lat.len() as u64;
                for v in lat {
                    let _ = h.record(v.max(1));
                }
            }
        }
        let secs = t0.elapsed().as_secs_f64();
        pool.close().await;
        DurableResult { latency: h, submits: n, per_second: n as f64 / secs }
    })
}
