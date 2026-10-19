//! task-io competitor harness. See `compare/README.md` and dev/BENCHMARKS.md.
//!
//! ```text
//! cargo run --release -- <suite|all> [--quick] [--host R1] [--only a,b] [--workers 1,4,16,32]
//! ```

mod alloc;
mod contenders;
mod cronjobs;
mod delayed;
mod durable;
mod jobs;
mod proto;
mod report;
mod retries;
mod stats;
mod stress;
mod sys;
mod workers;

use std::time::Duration;

use contenders::Contender;
use report::{Report, Row};

#[global_allocator]
static GLOBAL: alloc::Counting = alloc::Counting;

pub struct Opts {
    pub quick: bool,
    pub host: String,
    pub only: Option<Vec<String>>,
    pub workers: Vec<usize>,
}

impl Opts {
    fn wants(&self, name: &str) -> bool {
        self.only
            .as_ref()
            .is_none_or(|o| o.iter().any(|x| x == name))
    }
}

fn parse() -> (Vec<String>, Opts) {
    let mut suites = Vec::new();
    let mut o = Opts {
        quick: false,
        host: "local".into(),
        only: None,
        workers: vec![1, 4, 16, 32],
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--quick" => o.quick = true,
            "--host" => o.host = args.next().unwrap_or_default(),
            "--only" => {
                o.only = args
                    .next()
                    .map(|s| s.split(',').map(str::to_string).collect());
            }
            "--workers" => {
                o.workers = args
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(|s| s.parse().ok())
                    .collect();
            }
            s => suites.push(s.to_string()),
        }
    }
    if suites.is_empty() || suites.iter().any(|s| s == "all") {
        suites = [
            "W-01", "W-02", "W-03", "W-04", "W-05", "W-06", "W-08", "W-09", "W-10", "W-11", "W-12",
            "W-13", "D-01", "D-02", "P-01", "R-01", "R-02", "K-01",
        ]
        .map(String::from)
        .to_vec();
    }
    (suites, o)
}

fn main() {
    let (suites, o) = parse();
    let mut report = Report::new(&o.host, o.quick);
    for s in &suites {
        eprintln!("== {s}");
        match s.as_str() {
            "W-01" => w01(&o, &mut report),
            "W-02" => w02(&o, &mut report),
            "W-03" => w03(&o, &mut report),
            "W-04" => w04(&o, &mut report),
            "W-05" => w05(&o, &mut report),
            "W-06" => w06(&o, &mut report),
            "W-08" => w08(&o, &mut report),
            "W-09" => w09(&o, &mut report),
            "W-10" => w10(&o, &mut report),
            "W-11" => w11(&o, &mut report),
            "W-12" => w12(&o, &mut report),
            "W-13" => w13(&o, &mut report),
            "D-01" => d01(&o, &mut report),
            "D-02" => d02(&o, &mut report),
            "P-01" => p01(&o, &mut report),
            "R-01" => r01(&o, &mut report),
            "R-02" => r02(&o, &mut report),
            "K-01" => k01(&o, &mut report),
            other => eprintln!("unknown suite {other}"),
        }
    }
    report.finish();
}

/// apalis-core 1.0.0's in-memory backend panics when a push races the
/// worker's `poll_ready` (memory.rs:246, `try_lock().unwrap()` on the sink
/// both sides lock), so it cannot run the suites. This smoke check records the
/// failure on every run, and notices if a later apalis release fixes it.
fn apalis_smoke(o: &Opts, r: &mut Report) {
    if !o.wants("apalis") {
        return;
    }
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let res = std::panic::catch_unwind(|| workers::w01::<jobs::ApalisMemory>(4, 1, 200_000));
    std::panic::set_hook(hook);
    let v = "200000 empty jobs, 1 submitter (smoke check)".to_string();
    match res {
        Ok(secs) => r.push(Row::single("W-01", v, "apalis", 4, "wall time (in-memory backend ran; add it to the suites)", "ms", secs * 1000.0).info()),
        Err(_) => r.push(Row::missing("W-01", v, "apalis", 4, "panicked in apalis-core 1.0.0 memory.rs:246 (push races the worker's poll_ready); cannot be measured").info()),
    }
}

fn w01(o: &Opts, r: &mut Report) {
    apalis_smoke(o, r);
    let (jobs, runs) = if o.quick {
        (1_000_000, 3)
    } else {
        (10_000_000, 10)
    };
    for &w in &o.workers {
        for subs in [1usize, 32] {
            for_each_contender!(|n| o.wants(n), |C| {
                let samples: Vec<f64> = (0..runs)
                    .map(|_| workers::w01::<C>(w, subs, jobs))
                    .collect();
                r.push(Row::summary(
                    "W-01",
                    format!("{jobs} empty jobs, {subs} submitter(s)"),
                    C::NAME,
                    w,
                    "wall time",
                    "ms",
                    false,
                    &samples.iter().map(|s| s * 1000.0).collect::<Vec<_>>(),
                ));
            });
        }
    }
}

fn w02(o: &Opts, r: &mut Report) {
    let (idle_samples, busy_samples, runs) = if o.quick {
        (20_000, 200_000, 1)
    } else {
        (1_000_000, 1_000_000, 3)
    };
    let ws: Vec<usize> = o
        .workers
        .iter()
        .copied()
        .filter(|w| *w == 4 || *w == 32)
        .collect();
    let ws = if ws.is_empty() { o.workers.clone() } else { ws };
    for &w in &ws {
        for_each_contender!(|n| o.wants(n), |C| {
            let hs: Vec<_> = (0..runs)
                .map(|_| workers::w02_idle::<C>(w, idle_samples, Duration::from_micros(100)))
                .collect();
            r.push(Row::latency(
                "W-02",
                format!("idle pool, {idle_samples} samples"),
                C::NAME,
                w,
                &hs,
            ));
            let hs: Vec<_> = (0..runs)
                .map(|_| workers::w02_busy::<C>(w, busy_samples))
                .collect();
            r.push(Row::latency(
                "W-02",
                format!("80% busy, 10 us jobs, {busy_samples} samples"),
                C::NAME,
                w,
                &hs,
            ));
        });
    }
}

fn w03(o: &Opts, r: &mut Report) {
    let runs = if o.quick { 2 } else { 5 };
    for &w in &o.workers {
        for work_us in [10u64, 100, 1000] {
            for_each_contender!(|n| o.wants(n), |C| {
                let samples: Vec<f64> = (0..runs)
                    .map(|_| 100.0 * workers::w03::<C>(w, Duration::from_micros(work_us)))
                    .collect();
                r.push(Row::summary(
                    "W-03",
                    format!("{work_us} us busy jobs"),
                    C::NAME,
                    w,
                    "share of ideal throughput",
                    "%",
                    true,
                    &samples,
                ));
            });
        }
    }
}

fn w04(o: &Opts, r: &mut Report) {
    let jobs = if o.quick { 200_000 } else { 1_000_000 };
    let ws: Vec<usize> = o.workers.iter().copied().filter(|w| *w == 4).collect();
    let ws = if ws.is_empty() {
        vec![o.workers[0]]
    } else {
        ws
    };
    for &w in &ws {
        for_each_contender!(|n| o.wants(n), |C| {
            let m = workers::w04::<C>(w, jobs);
            r.push(Row::single(
                "W-04",
                format!("{jobs} queued jobs, 64-byte payload"),
                C::NAME,
                w,
                "effective bytes per queued job (growth + start reservation / queued)",
                "B",
                m.effective_bytes_per_job,
            ));
            r.push(
                Row::single(
                    "W-04",
                    format!("{jobs} queued jobs, 64-byte payload"),
                    C::NAME,
                    w,
                    "heap reserved at start",
                    "B",
                    m.reserved_at_start,
                )
                .info(),
            );
            r.push(
                Row::single(
                    "W-04",
                    format!("{jobs} queued jobs, 64-byte payload"),
                    C::NAME,
                    w,
                    "heap growth per queued job",
                    "B",
                    m.heap_bytes_per_job,
                )
                .info(),
            );
            r.push(Row::single(
                "W-04",
                format!("{jobs} queued jobs, 64-byte payload"),
                C::NAME,
                w,
                "allocations per submit",
                "",
                m.allocs_per_submit,
            ));
            if let Some(b) = C::reserved_bytes_per_slot() {
                r.push(
                    Row::single(
                        "W-04",
                        format!("{jobs} queued jobs, 64-byte payload"),
                        C::NAME,
                        w,
                        "preallocated bytes per queue slot (count with heap bytes)",
                        "B",
                        b as f64,
                    )
                    .info(),
                );
            }
            if m.queued < jobs {
                r.push(
                    Row::single(
                        "W-04",
                        format!("{jobs} queued jobs, 64-byte payload"),
                        C::NAME,
                        w,
                        "jobs it accepted (bounded queue)",
                        "",
                        m.queued as f64,
                    )
                    .info(),
                );
            }
            if let Some(rss) = m.rss_bytes_per_job {
                r.push(
                    Row::single(
                        "W-04",
                        format!("{jobs} queued jobs, 64-byte payload"),
                        C::NAME,
                        w,
                        "RSS bytes per queued job",
                        "B",
                        rss,
                    )
                    .info(),
                );
            }
        });
    }
}

fn w05(o: &Opts, r: &mut Report) {
    let idle = if o.quick {
        Duration::from_secs(5)
    } else {
        Duration::from_secs(60)
    };
    let ws: Vec<usize> = o.workers.iter().copied().filter(|w| *w == 32).collect();
    let ws = if ws.is_empty() {
        vec![*o.workers.last().unwrap_or(&4)]
    } else {
        ws
    };
    for &w in &ws {
        for_each_contender!(|n| o.wants(n), |C| {
            let m = workers::w05::<C>(w, idle);
            let v = format!("idle for {} s", idle.as_secs());
            match m.cpu_ms_per_s {
                Some(x) => r.push(Row::single(
                    "W-05",
                    v.clone(),
                    C::NAME,
                    w,
                    "CPU time per second",
                    "ms",
                    x,
                )),
                None => r.push(Row::missing(
                    "W-05",
                    v.clone(),
                    C::NAME,
                    w,
                    "CPU time per second",
                )),
            }
            match m.wakeups_per_s {
                Some(x) => r.push(Row::single(
                    "W-05",
                    v,
                    C::NAME,
                    w,
                    "context switches per second",
                    "",
                    x,
                )),
                None => r.push(
                    Row::missing(
                        "W-05",
                        v,
                        C::NAME,
                        w,
                        "context switches per second (not exposed on this OS)",
                    )
                    .info(),
                ),
            }
        });
    }
}

/// Worker counts for the stress suites: 4 and 32 when asked for, else the
/// first and last requested.
fn stress_workers(o: &Opts) -> Vec<usize> {
    let ws: Vec<usize> = o
        .workers
        .iter()
        .copied()
        .filter(|w| *w == 4 || *w == 32)
        .collect();
    if ws.is_empty() {
        let mut v = vec![o.workers[0]];
        if let Some(l) = o.workers.last().filter(|l| **l != o.workers[0]) {
            v.push(*l);
        }
        v
    } else {
        ws
    }
}

fn w06(o: &Opts, r: &mut Report) {
    let (dur, runs) = if o.quick {
        (Duration::from_secs(3), 1)
    } else {
        (Duration::from_secs(30), 3)
    };
    for &w in &stress_workers(o) {
        let res: Vec<_> = (0..runs).map(|_| stress::w06(w, dur)).collect();
        let v = format!("3 lanes, weights 4:2:1, saturated {} s", dur.as_secs());
        let errs: Vec<f64> = res.iter().map(|x| x.max_share_error_pct).collect();
        r.push(Row::summary(
            "W-06",
            v.clone(),
            "weighted-crossbeam",
            w,
            "max share error",
            "pp",
            false,
            &errs,
        ));
        let sh = res[0].shares_pct;
        let metric = format!(
            "top-lane share, run 1 (want 57.1; others {:.1} / {:.1}, want 28.6 / 14.3)",
            sh[1], sh[2]
        );
        r.push(Row::single(
            "W-06",
            v.clone(),
            "weighted-crossbeam",
            w,
            &metric,
            "%",
            sh[0],
        ));
        let hs: Vec<_> = res.into_iter().map(|x| x.top_lane).collect();
        r.push(Row::latency(
            "W-06",
            format!("{v}, top lane"),
            "weighted-crossbeam",
            w,
            &hs,
        ));
    }
}

fn w08(o: &Opts, r: &mut Report) {
    let (dur, runs) = if o.quick {
        (Duration::from_secs(3), 1)
    } else {
        (Duration::from_secs(60), 3)
    };
    for &w in &stress_workers(o) {
        for_each_contender!(|n| o.wants(n), |C| {
            let refs: Vec<_> = (0..runs)
                .map(|_| stress::w08::<C>(w, Duration::from_secs(5).min(dur), 0.5))
                .map(|x| x.admitted)
                .collect();
            r.push(Row::latency(
                "W-08",
                "50% load reference, 100 us jobs".into(),
                C::NAME,
                w,
                &refs,
            ));
            let res: Vec<_> = (0..runs).map(|_| stress::w08::<C>(w, dur, 2.0)).collect();
            let v = format!("2x capacity for {} s, 100 us jobs", dur.as_secs());
            let rss: Vec<f64> = res.iter().map(|x| x.peak_rss as f64 / 1e6).collect();
            r.push(Row::summary(
                "W-08",
                v.clone(),
                C::NAME,
                w,
                "peak RSS",
                "MB",
                false,
                &rss,
            ));
            let refused: Vec<f64> = res.iter().map(|x| x.refused_pct).collect();
            r.push(
                Row::summary(
                    "W-08",
                    v.clone(),
                    C::NAME,
                    w,
                    "refused",
                    "%",
                    false,
                    &refused,
                )
                .info(),
            );
            let hs: Vec<_> = res.into_iter().map(|x| x.admitted).collect();
            r.push(Row::latency("W-08", v, C::NAME, w, &hs));
        });
    }
}

fn w09(o: &Opts, r: &mut Report) {
    let (jobs, runs) = if o.quick {
        (200_000, 2)
    } else {
        (1_000_000, 5)
    };
    // Panic messages would dominate the run; silence them for every contender.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    for &w in &stress_workers(o) {
        for_each_contender!(|n| o.wants(n), |C| {
            let res: Vec<_> = (0..runs).map(|_| stress::w09::<C>(w, jobs)).collect();
            let v = format!("{jobs} jobs, 10% panic");
            let rate: Vec<f64> = res.iter().map(|x| x.jobs_per_s).collect();
            r.push(Row::summary(
                "W-09",
                v.clone(),
                C::NAME,
                w,
                "completed jobs per second",
                "/s",
                true,
                &rate,
            ));
            let lost = res.iter().map(|x| x.workers_lost).max().unwrap_or(0) as f64;
            r.push(Row::single("W-09", v, C::NAME, w, "workers lost (worst run)", "", lost).info());
        });
    }
    std::panic::set_hook(hook);
}

fn w10(o: &Opts, r: &mut Report) {
    let (dur, runs) = if o.quick {
        (Duration::from_secs(4), 1)
    } else {
        (Duration::from_secs(30), 3)
    };
    for &w in &stress_workers(o) {
        for_each_contender!(|n| o.wants(n), |C| {
            let hs: Vec<_> = (0..runs).map(|_| stress::w10::<C>(w, dur)).collect();
            let v = format!(
                "500 ms stuck job every second, 85% load, {} s",
                dur.as_secs()
            );
            r.push(Row::latency("W-10", v, C::NAME, w, &hs));
        });
    }
}

fn w11(o: &Opts, r: &mut Report) {
    let (queued, runs) = if o.quick {
        (200_000, 2)
    } else {
        (1_000_000, 5)
    };
    for &w in &stress_workers(o) {
        for_each_contender!(|n| o.wants(n), |C| {
            let res: Vec<_> = (0..runs).map(|_| stress::w11::<C>(w, queued)).collect();
            let v = format!("shutdown with {queued} queued jobs");
            let stop: Vec<f64> = res.iter().map(|x| x.stop_ms).collect();
            // Only a contender that ran every accepted job before stop returned
            // can set the bar; one that drops work is listed apart.
            // Comparable only if it held the whole queue and ran all of it.
            let kept_all = res
                .iter()
                .all(|x| x.queued == queued as u64 && x.completed == x.queued);
            let metric = if kept_all {
                "time to stop, every accepted job run"
            } else {
                "time to stop, not eligible (dropped jobs or could not hold the queue)"
            };
            r.push(Row::summary(
                "W-11",
                v.clone(),
                C::NAME,
                w,
                metric,
                "ms",
                false,
                &stop,
            ));
            let ran = res.iter().map(|x| x.completed).min().unwrap_or(0) as f64;
            let q = res.iter().map(|x| x.queued).min().unwrap_or(0) as f64;
            r.push(
                Row::single(
                    "W-11",
                    v.clone(),
                    C::NAME,
                    w,
                    "jobs it accepted into the queue",
                    "",
                    q,
                )
                .info(),
            );
            r.push(
                Row::single(
                    "W-11",
                    v.clone(),
                    C::NAME,
                    w,
                    "jobs run before stop returned (worst run)",
                    "",
                    ran,
                )
                .info(),
            );
            match res.iter().filter_map(|x| x.threads_left).max() {
                Some(t) => r.push(
                    Row::single(
                        "W-11",
                        v,
                        C::NAME,
                        w,
                        "threads left 200 ms after stop (worst run)",
                        "",
                        t as f64,
                    )
                    .info(),
                ),
                None => r.push(Row::missing("W-11", v, C::NAME, w, "threads left").info()),
            }
        });
    }
}

fn w12_rows(r: &mut Report, v: &str, name: &'static str, res: &[stress::StepResult]) {
    let p99: Vec<f64> = res.iter().map(|x| x.step_p99_ns as f64 / 1e6).collect();
    r.push(Row::summary(
        "W-12",
        v.to_string(),
        name,
        0,
        "p99 latency during the step",
        "ms",
        false,
        &p99,
    ));
    let settle: Vec<f64> = res.iter().map(|x| x.settle_s).collect();
    r.push(Row::summary(
        "W-12",
        v.to_string(),
        name,
        0,
        "time to settle",
        "s",
        false,
        &settle,
    ));
    let retire: Vec<f64> = res.iter().filter_map(|x| x.retire_s).collect();
    if retire.is_empty() {
        r.push(
            Row::missing(
                "W-12",
                v.to_string(),
                name,
                0,
                "time to retire extra threads",
            )
            .info(),
        );
    } else {
        r.push(
            Row::summary(
                "W-12",
                v.to_string(),
                name,
                0,
                "time to retire extra threads",
                "s",
                false,
                &retire,
            )
            .info(),
        );
    }
    if let Some(rev) = res.iter().filter_map(|x| x.reversals).max() {
        r.push(
            Row::single(
                "W-12",
                v.to_string(),
                name,
                0,
                "thread-count reversals (worst run)",
                "",
                rev as f64,
            )
            .info(),
        );
    }
    if let Some(p) = res.iter().filter_map(|x| x.peak_threads).max() {
        r.push(
            Row::single(
                "W-12",
                v.to_string(),
                name,
                0,
                "peak threads (worst run)",
                "",
                p as f64,
            )
            .info(),
        );
    }
}

fn w12(o: &Opts, r: &mut Report) {
    let ph = if o.quick {
        stress::StepPhases {
            base: Duration::from_secs(2),
            step: Duration::from_secs(6),
            after: Duration::from_secs(4),
        }
    } else {
        stress::StepPhases {
            base: Duration::from_secs(10),
            step: Duration::from_secs(60),
            after: Duration::from_secs(30),
        }
    };
    let runs = if o.quick { 1 } else { 3 };
    for h in [stress::Handler::Blocking, stress::Handler::Cpu] {
        let v = format!("load step 10x, {}", h.label());
        if o.wants("tokio-blocking-elastic") {
            let res: Vec<_> = (0..runs)
                .map(|_| stress::w12::<contenders::TokioBlockingElastic>(0, h, &ph))
                .collect();
            w12_rows(r, &v, "tokio-blocking-elastic", &res);
        }
        if o.wants("best-fixed") {
            let (size, first) = stress::w12_best_fixed(h, &ph);
            eprintln!("  best fixed size for {}: {size}", h.label());
            let mut res = vec![first];
            for _ in 1..runs {
                res.push(stress::w12::<contenders::CrossbeamPool>(size, h, &ph));
            }
            w12_rows(r, &v, "best-fixed (crossbeam-pool)", &res);
            r.push(
                Row::single(
                    "W-12",
                    v.clone(),
                    "best-fixed (crossbeam-pool)",
                    0,
                    "best fixed size found",
                    "workers",
                    size as f64,
                )
                .info(),
            );
        }
    }
}

fn w13(o: &Opts, r: &mut Report) {
    let runs = if o.quick { 1 } else { 3 };
    let w = 4usize;
    let v = "one handler hangs for 15 s; 85% load of 1 ms jobs; next 10 s".to_string();
    let rows = |r: &mut Report, name: &'static str, res: &[stress::FaultResult]| {
        let p99: Vec<f64> = res.iter().map(|x| x.p99_ns as f64 / 1e6).collect();
        r.push(Row::summary(
            "W-13",
            v.clone(),
            name,
            w,
            "p99 latency of other jobs",
            "ms",
            false,
            &p99,
        ));
        let late: Vec<f64> = res.iter().map(|x| x.late_jobs as f64).collect();
        r.push(Row::summary(
            "W-13",
            v.clone(),
            name,
            w,
            "jobs later than 100 ms",
            "",
            false,
            &late,
        ));
    };
    for_each_contender!(|n| o.wants(n), |C| {
        let res: Vec<_> = (0..runs).map(|_| stress::w13_hang::<C>(w)).collect();
        rows(r, C::NAME, &res);
    });
    if o.wants("tokio-blocking-elastic") {
        let res: Vec<_> = (0..runs)
            .map(|_| stress::w13_hang::<contenders::TokioBlockingElastic>(w))
            .collect();
        rows(r, "tokio-blocking-elastic", &res);
    }
}

fn d01(o: &Opts, r: &mut Report) {
    let (n, window, runs) = if o.quick {
        (100_000, Duration::from_secs(2), 1)
    } else {
        (1_000_000, Duration::from_secs(10), 3)
    };
    let w = 4usize;
    let v = format!(
        "{n} jobs delayed over {} s, 50% cancelled",
        window.as_secs()
    );
    let rows = |r: &mut Report, name: &'static str, res: Vec<delayed::DelayResult>| {
        let sub: Vec<f64> = res.iter().map(|x| x.ns_per_submit).collect();
        r.push(Row::summary(
            "D-01",
            v.clone(),
            name,
            w,
            "time per delayed submit",
            "ns",
            false,
            &sub,
        ));
        let can: Vec<f64> = res.iter().map(|x| x.ns_per_cancel).collect();
        r.push(Row::summary(
            "D-01",
            v.clone(),
            name,
            w,
            "time per cancel",
            "ns",
            false,
            &can,
        ));
        let early = res.iter().map(|x| x.early).max().unwrap_or(0) as f64;
        r.push(
            Row::single(
                "D-01",
                v.clone(),
                name,
                w,
                "jobs that ran early (worst run)",
                "",
                early,
            )
            .info(),
        );
        let late_cancel = res.iter().map(|x| x.fired_cancelled).max().unwrap_or(0) as f64;
        r.push(
            Row::single(
                "D-01",
                v.clone(),
                name,
                w,
                "jobs that ran after their cancel (worst run)",
                "",
                late_cancel,
            )
            .info(),
        );
        let hs: Vec<_> = res.into_iter().map(|x| x.lateness).collect();
        let mut row = Row::latency("D-01", v.clone(), name, w, &hs);
        row.metric = "firing lateness".into();
        r.push(row);
    };
    if o.wants("heap-scheduler") {
        let res: Vec<_> = (0..runs).map(|_| delayed::d01_heap(w, n, window)).collect();
        rows(r, "heap-scheduler", res);
    }
    if o.wants("tokio-timer") {
        let res: Vec<_> = (0..runs)
            .map(|_| delayed::d01_tokio(w, n, window))
            .collect();
        rows(r, "tokio-timer", res);
    }
}

fn p01(o: &Opts, r: &mut Report) {
    let (target, time_box, runs) = if o.quick {
        (5_000u64, Duration::from_secs(10), 1)
    } else {
        (1_000_000u64, Duration::from_secs(120), 3)
    };
    let dir = std::env::temp_dir().join("task-io-compare-p01");
    let _ = std::fs::create_dir_all(&dir);
    if !o.wants("apalis-sqlite") {
        return;
    }
    for subs in [1usize, 32] {
        for (sync, name) in [
            (durable::Sync::Full, "apalis-sqlite"),
            (
                durable::Sync::ApalisDefault,
                "apalis-sqlite (default, synchronous=OFF: not power-safe)",
            ),
        ] {
            let res: Vec<_> = (0..runs)
                .map(|_| durable::p01_apalis(&dir, sync, subs, target, time_box))
                .collect();
            let n = res.iter().map(|x| x.submits).min().unwrap_or(0);
            let v = format!(
                "durable submits, 64-byte payload, {subs} submitter(s), up to {target} or {} s (measured {n})",
                time_box.as_secs()
            );
            let rate: Vec<f64> = res.iter().map(|x| x.per_second).collect();
            r.push(Row::summary(
                "P-01",
                v.clone(),
                name,
                subs,
                "submits per second",
                "/s",
                true,
                &rate,
            ));
            let hs: Vec<_> = res.into_iter().map(|x| x.latency).collect();
            let mut row = Row::latency("P-01", v, name, subs, &hs);
            row.metric = "submit-to-durable latency".into();
            r.push(row);
        }
    }
}

fn d02(o: &Opts, r: &mut Report) {
    // Full protocol: 10,000 schedules firing every minute for 10 minutes.
    // Quick: 1,000 every second for 10 seconds.
    let (n, period, run_for, label) = if o.quick {
        (
            1_000,
            cronjobs::Period::Second,
            Duration::from_secs(10),
            "every second for 10 s",
        )
    } else {
        (
            10_000,
            cronjobs::Period::Minute,
            Duration::from_secs(600),
            "every minute for 10 min",
        )
    };
    let v = format!("{n} cron schedules, {label}");
    let rows = |r: &mut Report, name: &'static str, res: cronjobs::CronResult| {
        r.push(
            Row::single(
                "D-02",
                v.clone(),
                name,
                0,
                &format!("ticks fired (expected {})", res.expected),
                "",
                res.fired as f64,
            )
            .info(),
        );
        match res.cpu_ms {
            Some(c) => r.push(Row::single(
                "D-02",
                v.clone(),
                name,
                0,
                "CPU time over the run",
                "ms",
                c,
            )),
            None => r.push(Row::missing(
                "D-02",
                v.clone(),
                name,
                0,
                "CPU time over the run",
            )),
        }
        let mut row = Row::latency("D-02", v.clone(), name, 0, &[res.lateness]);
        row.metric = "firing lateness".into();
        r.push(row);
    };
    if o.wants("tokio-cron") {
        rows(r, "tokio-cron", cronjobs::d02_tokio(n, period, run_for));
    }
    if o.wants("apalis-cron") {
        rows(r, "apalis-cron", cronjobs::d02_apalis(n, period, run_for));
    }
}

fn rk_dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join("task-io-compare-rk");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn r01(o: &Opts, r: &mut Report) {
    if !o.wants("apalis-sqlite") {
        return;
    }
    let (n, time_box, runs) = if o.quick {
        (2_000u64, Duration::from_secs(20), 1)
    } else {
        (100_000u64, Duration::from_secs(120), 3)
    };
    let res: Vec<_> = (0..runs)
        .map(|_| retries::r01_apalis(&rk_dir(), n, time_box))
        .collect();
    let jobs = res.iter().map(|x| x.jobs).min().unwrap_or(0);
    let v = format!("10% fail once, backoff 10 ms to 1 s, {jobs} jobs (time-boxed submit)");
    if res.iter().all(|x| x.finished) {
        let rate: Vec<f64> = res.iter().map(|x| x.jobs_per_s).collect();
        r.push(Row::summary(
            "R-01",
            v,
            "apalis-sqlite",
            32,
            "completed jobs per second",
            "/s",
            true,
            &rate,
        ));
    } else {
        r.push(
            Row::missing(
                "R-01",
                v,
                "apalis-sqlite",
                32,
                "did not finish within 10 minutes",
            )
            .info(),
        );
    }
}

fn r02(o: &Opts, r: &mut Report) {
    if !o.wants("apalis-sqlite") {
        return;
    }
    let (n, outage, time_box, runs) = if o.quick {
        (1_000u64, Duration::from_secs(3), Duration::from_secs(20), 1)
    } else {
        (
            20_000u64,
            Duration::from_secs(10),
            Duration::from_secs(120),
            3,
        )
    };
    let res: Vec<_> = (0..runs)
        .map(|_| retries::r02_apalis(&rk_dir(), n, outage, time_box))
        .collect();
    let jobs = res.iter().map(|x| x.jobs).min().unwrap_or(0);
    let v = format!(
        "downstream down {} s, {jobs} jobs, 10 attempts, backoff 100 ms to 2 s",
        outage.as_secs()
    );
    let waste: Vec<f64> = res
        .iter()
        .map(|x| x.attempts_during_outage as f64)
        .collect();
    r.push(Row::summary(
        "R-02",
        v.clone(),
        "apalis-sqlite",
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
            "apalis-sqlite",
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
                "apalis-sqlite",
                32,
                "did not drain within 10 minutes",
            )
            .info(),
        );
    }
    let unaccounted = res
        .iter()
        .map(|x| x.jobs.saturating_sub(x.completed + x.dead))
        .max()
        .unwrap_or(0) as f64;
    r.push(
        Row::single(
            "R-02",
            v,
            "apalis-sqlite",
            32,
            "jobs neither completed nor recorded as failed (worst run)",
            "",
            unaccounted,
        )
        .info(),
    );
}

fn k01(o: &Opts, r: &mut Report) {
    if !o.wants("apalis-sqlite") {
        return;
    }
    let (n, time_box, runs) = if o.quick {
        (2_000u64, Duration::from_secs(20), 1)
    } else {
        (10_000_000u64, Duration::from_secs(120), 3)
    };
    let res: Vec<_> = (0..runs)
        .map(|_| retries::k01_apalis(&rk_dir(), 32, n, time_box))
        .collect();
    let subs = res.iter().map(|x| x.submits).min().unwrap_or(0);
    let v = format!("32 submitters, half the keys repeated, {subs} submits (time-boxed)");
    let keyed: Vec<f64> = res.iter().map(|x| x.ns_per_submit_keyed).collect();
    r.push(Row::summary(
        "K-01",
        v.clone(),
        "apalis-sqlite",
        32,
        "time per keyed submit",
        "ns",
        false,
        &keyed,
    ));
    let plain: Vec<f64> = res.iter().map(|x| x.ns_per_submit_plain).collect();
    r.push(
        Row::summary(
            "K-01",
            v.clone(),
            "apalis-sqlite",
            32,
            "time per plain submit (1 submitter, reference)",
            "ns",
            false,
            &plain,
        )
        .info(),
    );
    let dups = res.iter().map(|x| x.duplicate_runs).max().unwrap_or(0) as f64;
    r.push(
        Row::single(
            "K-01",
            v,
            "apalis-sqlite",
            32,
            "duplicate runs (worst run)",
            "",
            dups,
        )
        .info(),
    );
}
