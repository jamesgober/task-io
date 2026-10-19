//! fang contender for task-io's benchmarks (P-01, P-02, R-01, R-02, K-01), on
//! SQLite at full durability.
//!
//! ```text
//! cargo run --release -- <P-01|P-02|R-01|R-02|K-01|all> [--quick] [--host R1]
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

mod crash;
mod jobs;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fang::asynk::async_queue::AsyncQueueable;
use jobs::{FangJob, FangKeyed, Mode};
use report::{Report, Row};

/// Each run gets its own database file: on Windows the previous run's file
/// can still be open when the next run starts.
static RUN: AtomicU64 = AtomicU64::new(0);

fn db_dir() -> PathBuf {
    std::env::temp_dir().join("task-io-compare-fang")
}

fn db_file(tag: &str) -> String {
    let run = RUN.fetch_add(1, Ordering::Relaxed);
    format!("fang-{tag}-{}-{run}.db", std::process::id())
}

/// Inserts up to `n` jobs from one submitter (time-boxed); returns how many.
async fn push_jobs(
    q: &fang::asynk::async_queue::AsyncQueue,
    n: u64,
    mode: Mode,
    time_box: Duration,
) -> u64 {
    let t0 = Instant::now();
    let mut pushed = 0;
    for i in 0..n {
        if t0.elapsed() >= time_box || q.insert_task(&FangJob::new(i, mode)).await.is_err() {
            break;
        }
        pushed += 1;
    }
    pushed
}

fn p01(quick: bool, r: &mut Report) {
    let (target, time_box, runs) = if quick {
        (5_000u64, Duration::from_secs(10), 1)
    } else {
        (1_000_000u64, Duration::from_secs(120), 3)
    };
    for subs in [1usize, 32] {
        let mut rates = Vec::new();
        let mut hs = Vec::new();
        let mut measured = u64::MAX;
        for _ in 0..runs {
            let (rate, h, n) = jobs::runtime().block_on(async {
                let q = jobs::open_queue(&db_dir(), &db_file("p01"), true, subs as u32).await;
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
                            let job = FangJob::new(i, Mode::Effect);
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
    let mut rates = Vec::new();
    let mut finished_all = true;
    let mut dups = 0u64;
    for _ in 0..runs {
        jobs::reset();
        let (rate, finished) = jobs::runtime().block_on(async {
            let q = jobs::open_queue(&db_dir(), &db_file("r01"), true, 32).await;
            let jobs_in = push_jobs(&q, n, Mode::FailFirst, Duration::MAX).await;
            let t0 = Instant::now();
            jobs::start_workers(&q).await;
            let finished = jobs::wait_until(Duration::from_secs(600), jobs_in, || {
                jobs::state().runs.len() as u64
            })
            .await;
            (
                jobs::state().runs.len() as f64 / t0.elapsed().as_secs_f64(),
                finished,
            )
        });
        rates.push(rate);
        finished_all &= finished;
        dups = dups.max(jobs::state().duplicate_runs());
    }
    let v = format!("10% fail once, fang backoff 1 s (its minimum), {n} jobs");
    if finished_all {
        r.push(Row::summary(
            "R-01",
            v.clone(),
            "fang",
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
                "fang",
                32,
                "did not finish (no progress for 60 s, or past 10 minutes)",
            )
            .info(),
        );
    }
    r.push(
        Row::single(
            "R-01",
            v,
            "fang",
            32,
            "duplicate runs (worst run)",
            "",
            dups as f64,
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
    struct Out {
        jobs: u64,
        during: u64,
        drain_s: Option<f64>,
        unaccounted: u64,
        dups: u64,
    }
    let mut res = Vec::new();
    for _ in 0..runs {
        jobs::reset();
        let (jobs_in, drain_s) = jobs::runtime().block_on(async {
            let q = jobs::open_queue(&db_dir(), &db_file("r02"), true, 32).await;
            let jobs_in = push_jobs(&q, n, Mode::Outage, time_box).await;
            let until = Instant::now() + outage;
            jobs::state().outage_until = Some(until);
            jobs::start_workers(&q).await;
            let done = jobs::wait_until(outage + Duration::from_secs(600), jobs_in, || {
                jobs::state().accounted()
            })
            .await;
            let drain_s = done.then(|| {
                Instant::now()
                    .saturating_duration_since(until)
                    .as_secs_f64()
            });
            (jobs_in, drain_s)
        });
        let s = jobs::state();
        res.push(Out {
            jobs: jobs_in,
            during: s.attempts_during_outage,
            drain_s,
            unaccounted: jobs_in.saturating_sub(s.accounted()),
            dups: s.duplicate_runs(),
        });
    }
    let jobs = res.iter().map(|x| x.jobs).min().unwrap_or(0);
    let v = format!(
        "downstream down {} s, {jobs} jobs, 10 attempts, fang backoff 1 s then 2 s",
        outage.as_secs()
    );
    let waste: Vec<f64> = res.iter().map(|x| x.during as f64).collect();
    r.push(Row::summary(
        "R-02",
        v.clone(),
        "fang",
        32,
        "attempts made during the outage",
        "",
        false,
        &waste,
    ));
    let drains: Vec<f64> = res.iter().filter_map(|x| x.drain_s).collect();
    if drains.len() == res.len() {
        r.push(Row::summary(
            "R-02",
            v.clone(),
            "fang",
            32,
            "time to drain after recovery",
            "s",
            false,
            &drains,
        ));
    } else {
        r.push(
            Row::missing(
                "R-02",
                v.clone(),
                "fang",
                32,
                "did not drain (no progress for 60 s, or past 10 minutes)",
            )
            .info(),
        );
    }
    let unaccounted = res.iter().map(|x| x.unaccounted).max().unwrap_or(0);
    r.push(
        Row::single(
            "R-02",
            v.clone(),
            "fang",
            32,
            "jobs neither completed nor recorded as failed (worst run)",
            "",
            unaccounted as f64,
        )
        .info(),
    );
    let dups = res.iter().map(|x| x.dups).max().unwrap_or(0);
    r.push(
        Row::single(
            "R-02",
            v,
            "fang",
            32,
            "duplicate runs (worst run)",
            "",
            dups as f64,
        )
        .info(),
    );
}

fn k01(quick: bool, r: &mut Report) {
    let (n, time_box, runs) = if quick {
        (2_000u64, Duration::from_secs(20), 1)
    } else {
        (10_000_000u64, Duration::from_secs(120), 3)
    };
    let submitters = 32usize;
    struct Out {
        submits: u64,
        keyed_ns: f64,
        plain_ns: f64,
        stored_dups: u64,
        run_dups: u64,
    }
    let mut res = Vec::new();
    for _ in 0..runs {
        jobs::reset();
        let out = jobs::runtime().block_on(async {
            // Plain submits for the cost comparison.
            let q = jobs::open_queue(&db_dir(), &db_file("k01-plain"), true, 1).await;
            let t = Instant::now();
            let plain = push_jobs(&q, n, Mode::Effect, time_box).await;
            let plain_ns = t.elapsed().as_nanos() as f64 / plain.max(1) as f64;

            // Keyed submits: job i uses key i / 2, so every key is submitted
            // twice. fang's uniqueness is a lookup followed by an insert.
            let file = db_file("k01-keyed");
            let q = jobs::open_queue(&db_dir(), &file, true, submitters as u32).await;
            let next = Arc::new(AtomicU64::new(0));
            let submitted = Arc::new(AtomicU64::new(0));
            let t0 = Instant::now();
            let mut tasks = Vec::new();
            for _ in 0..submitters {
                let (q, next, submitted) = (q.clone(), next.clone(), submitted.clone());
                tasks.push(tokio::spawn(async move {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= n || t0.elapsed() >= time_box {
                            break;
                        }
                        let job = FangKeyed {
                            key: i / 2,
                            payload: vec![0; 64],
                        };
                        let _ = q.insert_task(&job).await;
                        submitted.fetch_add(1, Ordering::Relaxed);
                    }
                }));
            }
            for t in tasks {
                let _ = t.await;
            }
            let total = submitted.load(Ordering::Relaxed);
            let keyed_ns = t0.elapsed().as_nanos() as f64 * submitters as f64 / total.max(1) as f64;
            let keys = total.div_ceil(2);
            let stored = jobs::stored_rows(&db_dir(), &file).await;

            // Every distinct key should run once; give the workers time to drain.
            jobs::start_workers(&q).await;
            let _ = jobs::wait_until(Duration::from_secs(300), keys, || {
                jobs::state().runs.len() as u64
            })
            .await;
            tokio::time::sleep(Duration::from_secs(2)).await;
            Out {
                submits: total,
                keyed_ns,
                plain_ns,
                stored_dups: stored.saturating_sub(keys),
                run_dups: jobs::state().duplicate_runs(),
            }
        });
        res.push(out);
    }
    let subs = res.iter().map(|x| x.submits).min().unwrap_or(0);
    let v = format!("32 submitters, half the keys repeated, {subs} submits (time-boxed)");
    let keyed: Vec<f64> = res.iter().map(|x| x.keyed_ns).collect();
    r.push(Row::summary(
        "K-01",
        v.clone(),
        "fang",
        32,
        "time per keyed submit",
        "ns",
        false,
        &keyed,
    ));
    let plain: Vec<f64> = res.iter().map(|x| x.plain_ns).collect();
    r.push(
        Row::summary(
            "K-01",
            v.clone(),
            "fang",
            32,
            "time per plain submit (1 submitter, reference)",
            "ns",
            false,
            &plain,
        )
        .info(),
    );
    let stored = res.iter().map(|x| x.stored_dups).max().unwrap_or(0);
    r.push(
        Row::single(
            "K-01",
            v.clone(),
            "fang",
            32,
            "duplicate keyed tasks stored (worst run)",
            "",
            stored as f64,
        )
        .info(),
    );
    let dups = res.iter().map(|x| x.run_dups).max().unwrap_or(0);
    r.push(
        Row::single(
            "K-01",
            v,
            "fang",
            32,
            "duplicate runs (worst run)",
            "",
            dups as f64,
        )
        .info(),
    );
}

fn p02(quick: bool, r: &mut Report) {
    // BENCHMARKS asks for 1,000,000 pending jobs; fang on SQLite runs about a
    // thousand durable jobs a second, so its runs use fewer (the time-box
    // rule) and the row says how many.
    let (n, limit, runs) = if quick {
        (2_000u64, Duration::from_secs(120), 1)
    } else {
        (50_000u64, Duration::from_secs(900), 3)
    };
    let dir = std::env::temp_dir().join("task-io-compare-fang-p02");
    let res: Vec<_> = (0..runs).map(|_| crash::p02_fang(&dir, n, limit)).collect();
    let v = format!("kill -9 at half way, restart, {n} pending jobs");
    let resumes: Vec<f64> = res.iter().filter_map(|x| x.resume_s).collect();
    if resumes.len() == res.len() {
        r.push(Row::summary(
            "P-02",
            v.clone(),
            "fang",
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
                "fang",
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
            "fang",
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
            "fang",
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
                "fang",
                32,
                "not every job completed (stalled or past the limit)",
            )
            .info(),
        );
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    jobs::init_log();
    // Child processes for the crash suite (P-02).
    if argv.first().map(String::as_str) == Some("--child") {
        crash::child(&argv[1..]);
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
        suites = ["P-01", "P-02", "R-01", "R-02", "K-01"]
            .map(String::from)
            .to_vec();
    }
    let mut report = Report::new(&host, quick);
    for s in &suites {
        eprintln!("== {s}");
        jobs::take_logged_errors();
        jobs::take_worker_exits();
        // Suites that run fang's workers also report the store errors they hit.
        let worked: &'static str = match s.as_str() {
            "P-01" => {
                p01(quick, &mut report);
                continue;
            }
            "P-02" => {
                p02(quick, &mut report);
                // The workers ran in child processes; their errors went to
                // the children's stderr.
                continue;
            }
            "R-01" => {
                r01(quick, &mut report);
                "R-01"
            }
            "R-02" => {
                r02(quick, &mut report);
                "R-02"
            }
            "K-01" => {
                k01(quick, &mut report);
                "K-01"
            }
            other => {
                eprintln!("unknown suite {other}");
                continue;
            }
        };
        let errors = jobs::take_logged_errors();
        let exits = jobs::take_worker_exits();
        report.push(
            Row::single(
                worked,
                "workers lost: a store error after a job ends the worker, and fang restarts only panicked workers (all runs)".to_string(),
                "fang",
                32,
                "worker exits",
                "",
                exits as f64,
            )
            .info(),
        );
        report.push(
            Row::single(
                worked,
                "store errors logged by fang's workers (all runs)".to_string(),
                "fang",
                32,
                "logged errors",
                "",
                errors as f64,
            )
            .info(),
        );
    }
    report.finish();
}
