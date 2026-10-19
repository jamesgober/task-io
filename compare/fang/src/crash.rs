//! Suite P-02 (dev/BENCHMARKS.md section 4.5) for fang: kill -9 in the middle
//! of a run, restart, and check every job. Same method as the apalis version
//! in `compare/src/crash.rs`: the binary re-launches itself as a seed child,
//! a worker child that the parent kills (`Child::kill`), and a second worker
//! child; handlers append job ids to an effects file.
//!
//! fang only fetches tasks in state `new` or `retried`, so a task that was
//! `in_progress` when its process died is never picked up again. The run
//! therefore also stops once no new effect has landed for `STALL`, instead of
//! waiting out the whole limit.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fang::asynk::async_queue::AsyncQueueable;

use crate::jobs::{self, FangJob, Mode};

const DB: &str = "fang-p02.db";
/// No new effect for this long means the remaining jobs are stuck.
const STALL: Duration = Duration::from_secs(30);

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

/// Child entry point: `seed <dir> <n>` or `work <dir>`.
pub fn child(args: &[String]) {
    let rt = jobs::runtime();
    match args.first().map(String::as_str) {
        Some("seed") => {
            let dir = PathBuf::from(&args[1]);
            let n: u64 = args[2].parse().expect("job count");
            rt.block_on(async {
                let q = jobs::open_queue(&dir, DB, true, 8).await;
                // Seeding is setup, not measured. fang has no bulk insert, so
                // eight concurrent submitters.
                let next = std::sync::Arc::new(AtomicU64::new(0));
                let mut tasks = Vec::new();
                for _ in 0..8 {
                    let (q, next) = (q.clone(), next.clone());
                    tasks.push(tokio::spawn(async move {
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            if i >= n {
                                break;
                            }
                            q.insert_task(&FangJob::new(i, Mode::Effect))
                                .await
                                .expect("seed insert");
                        }
                    }));
                }
                for t in tasks {
                    t.await.expect("seed task");
                }
            });
        }
        Some("work") => {
            let dir = PathBuf::from(&args[1]);
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(effects_path(&dir))
                .expect("effects file");
            let _ = jobs::EFFECTS.set(Mutex::new(file));
            rt.block_on(async {
                let q = jobs::open_queue(&dir, DB, false, 32).await;
                jobs::start_workers(&q).await;
                // Runs until the parent kills the process.
                loop {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                }
            });
        }
        _ => eprintln!("unknown child mode"),
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
pub fn p02_fang(dir: &Path, n: u64, limit: Duration) -> CrashResult {
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
    let (mut last_len, mut last_change) = (before, Instant::now());
    while t1.elapsed() < limit && last_change.elapsed() < STALL {
        let e = effects(dir);
        if resume_s.is_none() && e.len() > before {
            resume_s = Some(t1.elapsed().as_secs_f64());
        }
        if e.len() != last_len {
            (last_len, last_change) = (e.len(), Instant::now());
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
