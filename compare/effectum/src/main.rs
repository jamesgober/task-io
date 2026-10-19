//! effectum contender for task-io's benchmarks (P-01, P-02, R-01, R-02), on
//! SQLite.
//!
//! ```text
//! cargo run --release -- <P-01|P-02|R-01|R-02|all> [--quick] [--host R1]
//! ```
//!
//! effectum 0.7 opens its database with `PRAGMA journal = wal`, which is not
//! a SQLite pragma (the name is `journal_mode`), and SQLite ignores unknown
//! pragmas, so the database stays in rollback-journal mode; its writer sets
//! `synchronous = NORMAL`. That is the configuration a user gets, so it is
//! what the gated rows measure, and the rows say so. A second P-01 row
//! enables WAL before effectum opens the file (WAL persists in the file), the
//! mode its code intends; with `synchronous = NORMAL`, WAL commits can be
//! lost on power loss, so that row is a labelled reference only.
//!
//! effectum has no idempotency keys, so K-01 does not apply. Payloads are raw
//! bytes (its cheapest form); jobs run with 32-way concurrency, as for the
//! other durable contenders.

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

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use effectum::{Job, JobRunner, Queue, RunningJob, Worker};
use report::{Report, Row};

const JOB: &str = "bench";
/// No progress for this long means the remaining jobs are stuck.
const STALL: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Jobs and the shared run state.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Mode {
    /// R-01: every tenth job fails its first attempt.
    FailFirst = 1,
    /// R-02: every attempt fails while the outage lasts.
    Outage = 2,
    /// P-01 and P-02: append the id to the effects file (when one is set).
    Effect = 3,
}

/// 8-byte id, 1-byte mode, 64-byte payload.
fn payload(i: u64, mode: Mode) -> Vec<u8> {
    let mut p = Vec::with_capacity(73);
    p.extend_from_slice(&i.to_le_bytes());
    p.push(mode as u8);
    p.extend_from_slice(&[(i & 0xff) as u8; 64]);
    p
}

const OUTAGE_ATTEMPTS: u32 = 10;

#[derive(Default)]
struct State {
    runs: HashMap<u64, u32>,
    attempts: HashMap<u64, u32>,
    dead: HashSet<u64>,
    outage_until: Option<Instant>,
    attempts_during_outage: u64,
}

impl State {
    fn duplicate_runs(&self) -> u64 {
        self.runs
            .values()
            .map(|&c| u64::from(c.saturating_sub(1)))
            .sum()
    }
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
static EFFECTS: OnceLock<Mutex<File>> = OnceLock::new();

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn reset() {
    *state() = State::default();
}

#[derive(Clone, Debug)]
struct Ctx;

async fn bench(job: RunningJob, _ctx: Ctx) -> Result<(), String> {
    let p = &job.payload;
    if p.len() < 9 {
        return Err("short payload".into());
    }
    let i = u64::from_le_bytes(p[..8].try_into().map_err(|_| "id")?);
    match p[8] {
        1 => {
            let mut s = state();
            let a = s.attempts.entry(i).or_insert(0);
            *a += 1;
            if i.is_multiple_of(10) && *a == 1 {
                return Err("temporary failure (injected)".into());
            }
            *s.runs.entry(i).or_insert(0) += 1;
        }
        2 => {
            let mut s = state();
            let a = s.attempts.entry(i).or_insert(0);
            *a += 1;
            let a = *a;
            if s.outage_until.is_some_and(|t| Instant::now() < t) {
                s.attempts_during_outage += 1;
                if a >= OUTAGE_ATTEMPTS {
                    s.dead.insert(i);
                }
                return Err("downstream unavailable (injected)".into());
            }
            *s.runs.entry(i).or_insert(0) += 1;
        }
        _ => {
            if let Some(f) = EFFECTS.get()
                && let Ok(mut f) = f.lock()
            {
                let _ = f.write_all(&i.to_le_bytes());
            }
            *state().runs.entry(i).or_insert(0) += 1;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Queue setup.
// ---------------------------------------------------------------------------

static RUN: AtomicU64 = AtomicU64::new(0);

fn db_dir() -> PathBuf {
    std::env::temp_dir().join("task-io-compare-effectum")
}

/// A fresh database path for one run (Windows can keep the last one open).
fn fresh_db(tag: &str) -> PathBuf {
    let dir = db_dir();
    let _ = std::fs::create_dir_all(&dir);
    let run = RUN.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("effectum-{tag}-{}-{run}.db", std::process::id()));
    remove_db(&path);
    path
}

fn remove_db(path: &Path) {
    for ext in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
    }
}

/// Puts a new database file in WAL mode before effectum opens it.
fn preset_wal(path: &Path) {
    let conn = rusqlite::Connection::open(path).expect("open db");
    let mode: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .expect("set journal_mode");
    assert_eq!(mode.to_lowercase(), "wal", "WAL not enabled");
}

/// Jobs effectum has marked as failed for good, read from its own table.
fn failed_in_db(path: &Path) -> u64 {
    rusqlite::Connection::open(path)
        .and_then(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM jobs WHERE status = 'failed'",
                [],
                |r| r.get::<_, i64>(0),
            )
        })
        .map_or(0, |n| n.max(0) as u64)
}

/// `failed_in_db`, read at most every 500 ms: effectum's database is in
/// rollback-journal mode, where a reader blocks its writer.
fn failed_poller(path: PathBuf) -> impl Fn() -> u64 {
    let last = Mutex::new((Instant::now() - Duration::from_secs(1), 0u64));
    move || {
        let mut g = last.lock().unwrap_or_else(|e| e.into_inner());
        if g.0.elapsed() >= Duration::from_millis(500) {
            *g = (Instant::now(), failed_in_db(&path));
        }
        g.1
    }
}

fn journal_mode(path: &Path) -> String {
    rusqlite::Connection::open(path)
        .and_then(|c| c.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0)))
        .unwrap_or_else(|_| "unknown".into())
}

async fn start_worker(q: &Queue) -> Worker {
    Worker::builder(q, Ctx)
        .jobs(vec![JobRunner::builder(JOB, bench).build()])
        .min_concurrency(32)
        .max_concurrency(32)
        .build()
        .await
        .expect("effectum worker")
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .expect("rt")
}

/// Waits until `done()` reaches `target`, `limit` passes, or `done()` has not
/// moved for `STALL`.
async fn wait_until(limit: Duration, target: u64, done: impl Fn() -> u64) -> bool {
    let t0 = Instant::now();
    let (mut last, mut moved) = (done(), Instant::now());
    while last < target {
        if t0.elapsed() > limit || moved.elapsed() > STALL {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
        let now = done();
        if now != last {
            (last, moved) = (now, Instant::now());
        }
    }
    true
}

fn job(i: u64, mode: Mode) -> effectum::JobBuilder {
    Job::builder(JOB).payload(payload(i, mode))
}

/// Adds up to `n` jobs from one submitter (time-boxed); returns how many.
async fn push_jobs(
    q: &Queue,
    n: u64,
    time_box: Duration,
    make: impl Fn(u64) -> effectum::JobBuilder,
) -> u64 {
    let t0 = Instant::now();
    let mut pushed = 0;
    for i in 0..n {
        if t0.elapsed() >= time_box || make(i).add_to(q).await.is_err() {
            break;
        }
        pushed += 1;
    }
    pushed
}

// ---------------------------------------------------------------------------
// P-01: durable submits.
// ---------------------------------------------------------------------------

fn p01(quick: bool, r: &mut Report) {
    let (target, time_box, runs) = if quick {
        (5_000u64, Duration::from_secs(10), 1)
    } else {
        (1_000_000u64, Duration::from_secs(120), 3)
    };
    for wal in [false, true] {
        for subs in [1usize, 32] {
            let mut rates = Vec::new();
            let mut hs = Vec::new();
            let mut measured = u64::MAX;
            let mut mode = String::new();
            for _ in 0..runs {
                let path = fresh_db("p01");
                if wal {
                    preset_wal(&path);
                }
                let (rate, h, n) = runtime().block_on(async {
                    let q = Arc::new(Queue::new(&path).await.expect("effectum queue"));
                    let next = Arc::new(AtomicU64::new(0));
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
                                let t = Instant::now();
                                if job(i, Mode::Effect).add_to(&q).await.is_err() {
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
                    let rate = n as f64 / t0.elapsed().as_secs_f64();
                    let _ = q.close(Duration::from_secs(10)).await;
                    (rate, h, n)
                });
                mode = journal_mode(&path);
                rates.push(rate);
                hs.push(h);
                measured = measured.min(n);
            }
            let setup = if wal {
                "WAL set before effectum opens the file (reference: synchronous NORMAL loses WAL commits on power loss)"
            } else {
                "as shipped"
            };
            let v = format!(
                "durable submits, 64-byte payload, {subs} submitter(s), up to {target} or {} s (measured {measured}); {setup}; journal_mode {mode}, synchronous NORMAL",
                time_box.as_secs()
            );
            let mut rate_row = Row::summary(
                "P-01",
                v.clone(),
                "effectum",
                subs,
                "submits per second",
                "/s",
                true,
                &rates,
            );
            let mut lat_row = Row::latency("P-01", v, "effectum", subs, &hs);
            lat_row.metric = "submit-to-durable latency".into();
            if wal {
                rate_row = rate_row.info();
                lat_row = lat_row.info();
            }
            r.push(rate_row);
            r.push(lat_row);
        }
    }
}

// ---------------------------------------------------------------------------
// R-01 and R-02: retries.
// ---------------------------------------------------------------------------

fn r01(quick: bool, r: &mut Report) {
    let (n, runs) = if quick {
        (2_000u64, 1)
    } else {
        (100_000u64, 3)
    };
    let mut rates = Vec::new();
    let mut finished_all = true;
    let mut dups = 0u64;
    let mut failed = 0u64;
    for _ in 0..runs {
        reset();
        let path = fresh_db("r01");
        let poll = failed_poller(path.clone());
        let (rate, finished) = runtime().block_on(async {
            let q = Queue::new(&path).await.expect("effectum queue");
            let jobs = push_jobs(&q, n, Duration::MAX, |i| {
                job(i, Mode::FailFirst)
                    .max_retries(3)
                    .backoff_initial_interval(Duration::from_millis(10))
                    .backoff_multiplier(2.0)
            })
            .await;
            let t0 = Instant::now();
            let _worker = start_worker(&q).await;
            // Stops early once every job is completed or failed for good.
            let _ = wait_until(Duration::from_secs(600), jobs, || {
                state().runs.len() as u64 + poll()
            })
            .await;
            let rate = state().runs.len() as f64 / t0.elapsed().as_secs_f64();
            let _ = q.close(Duration::from_secs(10)).await;
            (rate, state().runs.len() as u64 >= jobs)
        });
        rates.push(rate);
        finished_all &= finished;
        dups = dups.max(state().duplicate_runs());
        failed = failed.max(failed_in_db(&path));
    }
    let v = format!("10% fail once, backoff 10 ms doubling, {n} jobs");
    if finished_all {
        r.push(Row::summary(
            "R-01",
            v.clone(),
            "effectum",
            32,
            "completed jobs per second",
            "/s",
            true,
            &rates,
        ));
    } else {
        r.push(
            Row::missing(
                "R-01",
                v.clone(),
                "effectum",
                32,
                "did not finish (no progress for 60 s, or past 10 minutes)",
            )
            .info(),
        );
    }
    r.push(
        Row::single(
            "R-01",
            v.clone(),
            "effectum",
            32,
            "duplicate runs (worst run)",
            "",
            dups as f64,
        )
        .info(),
    );
    // Every injected failure is a single one, so any job failed for good
    // is one the retry policy should have saved.
    r.push(
        Row::single(
            "R-01",
            v,
            "effectum",
            32,
            "jobs failed for good after one injected failure (worst run)",
            "",
            failed as f64,
        )
        .info(),
    );
}

fn r02(quick: bool, r: &mut Report) {
    let (n, outage, time_box, runs) = if quick {
        (1_000u64, Duration::from_secs(3), Duration::from_secs(20), 1)
    } else {
        (
            20_000u64,
            Duration::from_secs(10),
            Duration::from_secs(120),
            3,
        )
    };
    let mut res = Vec::new();
    for _ in 0..runs {
        reset();
        let path = fresh_db("r02");
        let poll = failed_poller(path.clone());
        let (jobs, drain_s) = runtime().block_on(async {
            let q = Queue::new(&path).await.expect("effectum queue");
            let jobs = push_jobs(&q, n, time_box, |i| {
                job(i, Mode::Outage)
                    .max_retries(OUTAGE_ATTEMPTS - 1)
                    .backoff_initial_interval(Duration::from_millis(100))
                    .backoff_multiplier(2.0)
            })
            .await;
            let until = Instant::now() + outage;
            state().outage_until = Some(until);
            let _worker = start_worker(&q).await;
            let done = wait_until(outage + Duration::from_secs(600), jobs, || {
                state().runs.len() as u64 + poll()
            })
            .await;
            let drain_s = done.then(|| {
                Instant::now()
                    .saturating_duration_since(until)
                    .as_secs_f64()
            });
            let _ = q.close(Duration::from_secs(10)).await;
            (jobs, drain_s)
        });
        let failed = failed_in_db(&path);
        let s = state();
        res.push((
            jobs,
            s.attempts_during_outage,
            drain_s,
            jobs.saturating_sub(s.runs.len() as u64 + failed),
            s.duplicate_runs(),
            failed.saturating_sub(s.dead.len() as u64),
        ));
    }
    let jobs = res.iter().map(|x| x.0).min().unwrap_or(0);
    let v = format!(
        "downstream down {} s, {jobs} jobs, 10 attempts, backoff 100 ms doubling (effectum has no cap)",
        outage.as_secs()
    );
    let waste: Vec<f64> = res.iter().map(|x| x.1 as f64).collect();
    r.push(Row::summary(
        "R-02",
        v.clone(),
        "effectum",
        32,
        "attempts made during the outage",
        "",
        false,
        &waste,
    ));
    // A job should fail for good only after its tenth failed attempt; a drain
    // time from runs that failed jobs early does not measure a drain.
    let early = res.iter().map(|x| x.5).max().unwrap_or(0);
    let drains: Vec<f64> = res.iter().filter_map(|x| x.2).collect();
    if drains.len() == res.len() {
        let row = Row::summary(
            "R-02",
            v.clone(),
            "effectum",
            32,
            "time to drain after recovery",
            "s",
            false,
            &drains,
        );
        r.push(if early > 0 { row.info() } else { row });
    } else {
        r.push(
            Row::missing(
                "R-02",
                v.clone(),
                "effectum",
                32,
                "did not drain (no progress for 60 s, or past 10 minutes)",
            )
            .info(),
        );
    }
    let unaccounted = res.iter().map(|x| x.3).max().unwrap_or(0);
    r.push(
        Row::single(
            "R-02",
            v.clone(),
            "effectum",
            32,
            "jobs neither completed nor recorded as failed (worst run)",
            "",
            unaccounted as f64,
        )
        .info(),
    );
    let dups = res.iter().map(|x| x.4).max().unwrap_or(0);
    r.push(
        Row::single(
            "R-02",
            v.clone(),
            "effectum",
            32,
            "duplicate runs (worst run)",
            "",
            dups as f64,
        )
        .info(),
    );
    r.push(
        Row::single(
            "R-02",
            v,
            "effectum",
            32,
            "jobs failed for good before their tenth attempt (worst run)",
            "",
            early as f64,
        )
        .info(),
    );
}

// ---------------------------------------------------------------------------
// P-02: kill -9 half way, restart (same method as compare/src/crash.rs).
// effectum's default recovery marks jobs left running as failed and retries
// them at once.
// ---------------------------------------------------------------------------

const P02_DB: &str = "effectum-p02.db";

fn effects_path(dir: &Path) -> PathBuf {
    dir.join("effects.bin")
}

fn effects(dir: &Path) -> Vec<u64> {
    std::fs::read(effects_path(dir))
        .unwrap_or_default()
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| u64::from_le_bytes(*c))
        .collect()
}

fn child(args: &[String]) {
    let rt = runtime();
    match args.first().map(String::as_str) {
        Some("seed") => {
            let dir = PathBuf::from(&args[1]);
            let n: u64 = args[2].parse().expect("job count");
            let path = dir.join(P02_DB);
            remove_db(&path);
            rt.block_on(async {
                let q = Queue::new(&path).await.expect("effectum queue");
                // Seeding is setup, not measured: one batch per 1,000.
                let mut i = 0;
                while i < n {
                    let batch: Vec<Job> = (i..(i + 1_000).min(n))
                        .map(|k| job(k, Mode::Effect).build())
                        .collect();
                    i += batch.len() as u64;
                    q.add_jobs(batch).await.expect("seed jobs");
                }
                let _ = q.close(Duration::from_secs(10)).await;
            });
        }
        Some("work") => {
            let dir = PathBuf::from(&args[1]);
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(effects_path(&dir))
                .expect("effects file");
            let _ = EFFECTS.set(Mutex::new(file));
            rt.block_on(async {
                let q = Queue::new(&dir.join(P02_DB)).await.expect("effectum queue");
                let _worker = start_worker(&q).await;
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

struct CrashResult {
    resume_s: Option<f64>,
    lost: u64,
    duplicates: u64,
    finished: bool,
}

fn p02_once(dir: &Path, n: u64, limit: Duration) -> CrashResult {
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::remove_file(effects_path(dir));
    let d = dir.to_string_lossy().to_string();
    let ns = n.to_string();
    let status = spawn_child(&["seed", &d, &ns]).wait().expect("seed child");
    assert!(status.success(), "seed child failed");

    let mut worker = spawn_child(&["work", &d]);
    let t0 = Instant::now();
    while (effects(dir).len() as u64) < n / 2 && t0.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = worker.kill();
    let _ = worker.wait();
    let before = effects(dir).len();

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

fn p02(quick: bool, r: &mut Report) {
    let (n, limit, runs) = if quick {
        (2_000u64, Duration::from_secs(120), 1)
    } else {
        (50_000u64, Duration::from_secs(900), 3)
    };
    let dir = std::env::temp_dir().join("task-io-compare-effectum-p02");
    let res: Vec<_> = (0..runs).map(|_| p02_once(&dir, n, limit)).collect();
    let v = format!("kill -9 at half way, restart, {n} pending jobs");
    let resumes: Vec<f64> = res.iter().filter_map(|x| x.resume_s).collect();
    if resumes.len() == res.len() {
        r.push(Row::summary(
            "P-02",
            v.clone(),
            "effectum",
            32,
            "time to resume after restart",
            "s",
            false,
            &resumes,
        ));
    } else {
        r.push(
            Row::missing(
                "P-02",
                v.clone(),
                "effectum",
                32,
                "did not resume within the limit",
            )
            .info(),
        );
    }
    let lost = res.iter().map(|x| x.lost).max().unwrap_or(0);
    r.push(
        Row::single(
            "P-02",
            v.clone(),
            "effectum",
            32,
            "lost jobs (worst run)",
            "",
            lost as f64,
        )
        .info(),
    );
    let dups = res.iter().map(|x| x.duplicates).max().unwrap_or(0);
    r.push(
        Row::single(
            "P-02",
            v.clone(),
            "effectum",
            32,
            "duplicate effects (worst run)",
            "",
            dups as f64,
        )
        .info(),
    );
    if !res.iter().all(|x| x.finished) {
        r.push(
            Row::missing(
                "P-02",
                v,
                "effectum",
                32,
                "not every job completed (stalled or past the limit)",
            )
            .info(),
        );
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("--child") {
        child(&argv[1..]);
        return;
    }
    let mut quick = false;
    let mut host = "local".to_string();
    let mut suites = Vec::new();
    let mut args = argv.into_iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--quick" => quick = true,
            "--host" => host = args.next().unwrap_or_default(),
            s => suites.push(s.to_string()),
        }
    }
    if suites.is_empty() || suites.iter().any(|s| s == "all") {
        suites = ["P-01", "P-02", "R-01", "R-02"].map(String::from).to_vec();
    }
    let mut report = Report::new(&host, quick);
    for s in &suites {
        eprintln!("== {s}");
        match s.as_str() {
            "P-01" => p01(quick, &mut report),
            "P-02" => p02(quick, &mut report),
            "R-01" => r01(quick, &mut report),
            "R-02" => r02(quick, &mut report),
            other => eprintln!("unknown suite {other}"),
        }
    }
    report.finish();
}
