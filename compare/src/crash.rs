//! Suite P-02 (dev/BENCHMARKS.md section 4.5): kill -9 in the middle of a run,
//! restart, and check every job, against apalis on SQLite at full durability.
//!
//! The harness re-launches itself as a child process (`compare --child ...`):
//! one child seeds the pending jobs, a second runs the worker until the parent
//! kills it (`Child::kill`: `TerminateProcess` on Windows, `SIGKILL` on
//! Unix), and a third resumes. Each handler appends the job id to an effects
//! file with a plain write; a killed process loses nothing the OS already
//! has, so the file is exact for a process crash (not for power loss, which
//! P-02 does not claim to model).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use apalis::prelude::*;
use apalis_sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteStorage};

use crate::durable::DurableJob;

pub struct CrashResult {
    pub resume_s: Option<f64>,
    pub lost: u64,
    pub duplicates: u64,
    pub finished: bool,
}

fn effects_path(dir: &Path) -> PathBuf {
    dir.join("effects.bin")
}

fn effects(dir: &Path) -> Vec<u64> {
    std::fs::read(effects_path(dir))
        .unwrap_or_default()
        .chunks_exact(8)
        .map(|c| u64::from_le_bytes(c.try_into().expect("8 bytes")))
        .collect()
}

async fn pool(dir: &Path, fresh: bool) -> SqlitePool {
    let path = dir.join("apalis-p02.db");
    if fresh {
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
        }
    }
    let url = format!(
        "sqlite://{}?mode=rwc",
        path.display().to_string().replace('\\', "/")
    );
    let base = SqliteConnectOptions::from_str(&url).expect("sqlite url");
    if fresh {
        let setup = SqlitePool::connect_with(base.clone())
            .await
            .expect("sqlite connect");
        SqliteStorage::<()>::setup(&setup)
            .await
            .expect("apalis setup");
        setup.close().await;
    }
    SqlitePoolOptions::new()
        .max_connections(32)
        .connect_with(
            base.pragma("journal_mode", "WAL")
                .pragma("synchronous", "FULL"),
        )
        .await
        .expect("sqlite pool")
}

/// Child entry point: `seed <dir> <n>` or `work <dir>`.
pub fn child(args: &[String]) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .expect("rt");
    match args.first().map(String::as_str) {
        Some("seed") => {
            let dir = PathBuf::from(&args[1]);
            let n: u64 = args[2].parse().expect("job count");
            rt.block_on(async {
                let pool = pool(&dir, true).await;
                let mut storage = SqliteStorage::<DurableJob>::new(&pool);
                // Seeding is setup, not measured: one bulk push per 1,000.
                let mut i = 0;
                while i < n {
                    let batch: Vec<DurableJob> = (i..(i + 1_000).min(n))
                        .map(|k| DurableJob {
                            i: k,
                            payload: vec![(k & 0xff) as u8; 64],
                        })
                        .collect();
                    i += batch.len() as u64;
                    storage.push_bulk(batch).await.expect("seed push");
                }
                pool.close().await;
            });
        }
        Some("work") => {
            let dir = PathBuf::from(&args[1]);
            rt.block_on(async {
                let pool = pool(&dir, false).await;
                let storage = SqliteStorage::<DurableJob>::new(&pool);
                let file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(effects_path(&dir))
                    .expect("effects file");
                let file = Arc::new(Mutex::new(file));
                let worker = WorkerBuilder::new(format!("p02-{}", std::process::id()))
                    .backend(storage)
                    .concurrency(32)
                    .data(file)
                    .build(effect);
                if let Err(e) = worker.run().await {
                    eprintln!("p02 worker stopped with an error: {e}");
                }
            });
        }
        _ => eprintln!("unknown child mode"),
    }
}

async fn effect(job: DurableJob, file: Data<Arc<Mutex<std::fs::File>>>) {
    if let Ok(mut f) = file.lock() {
        let _ = f.write_all(&job.i.to_le_bytes());
    }
}

fn spawn_child(args: &[&str]) -> Child {
    let exe = std::env::current_exe().expect("current exe");
    Command::new(exe)
        .arg("--child")
        .args(args)
        .spawn()
        .expect("spawn child")
}

/// One crash run with `n` pending jobs.
pub fn p02_apalis(dir: &Path, n: u64, limit: Duration) -> CrashResult {
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::remove_file(effects_path(dir));
    let d = dir.to_string_lossy().to_string();
    let ns = n.to_string();
    let status = spawn_child(&["seed", &d, &ns]).wait().expect("seed child");
    assert!(status.success(), "seed child failed");

    // Run until about half the jobs have taken effect, then kill -9.
    let mut worker = spawn_child(&["work", &d]);
    let t0 = Instant::now();
    while (effects(dir).len() as u64) < n / 2 && t0.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = worker.kill();
    let _ = worker.wait();
    let before = effects(dir).len();

    // Restart and time the first new effect.
    let mut worker = spawn_child(&["work", &d]);
    let t1 = Instant::now();
    let mut resume_s = None;
    let mut finished = false;
    while t1.elapsed() < limit {
        let e = effects(dir);
        if resume_s.is_none() && e.len() > before {
            resume_s = Some(t1.elapsed().as_secs_f64());
        }
        let mut seen = vec![false; n as usize];
        for id in &e {
            if let Some(s) = seen.get_mut(*id as usize) {
                *s = true;
            }
        }
        if seen.iter().all(|s| *s) {
            finished = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Let in-flight effects land, then stop the worker.
    std::thread::sleep(Duration::from_millis(500));
    let _ = worker.kill();
    let _ = worker.wait();

    let e = effects(dir);
    let mut count = vec![0u32; n as usize];
    for id in &e {
        if let Some(c) = count.get_mut(*id as usize) {
            *c += 1;
        }
    }
    CrashResult {
        resume_s,
        lost: count.iter().filter(|c| **c == 0).count() as u64,
        duplicates: count.iter().map(|c| u64::from(c.saturating_sub(1))).sum(),
        finished,
    }
}
