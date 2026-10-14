//! task-io competitor harness. See `compare/README.md` and dev/BENCHMARKS.md.
//!
//! ```text
//! cargo run --release -- <suite|all> [--quick] [--host R1] [--only a,b] [--workers 1,4,16,32]
//! ```

mod alloc;
mod contenders;
mod report;
mod stats;
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
        suites = ["W-01", "W-02", "W-03", "W-04", "W-05"]
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
            other => eprintln!("unknown suite {other}"),
        }
    }
    report.finish();
}

fn w01(o: &Opts, r: &mut Report) {
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
                "heap bytes per queued job",
                "B",
                m.heap_bytes_per_job,
            ));
            r.push(Row::single(
                "W-04",
                format!("{jobs} queued jobs, 64-byte payload"),
                C::NAME,
                w,
                "allocations per submit",
                "",
                m.allocs_per_submit,
            ));
            if let Some(rss) = m.rss_bytes_per_job {
                r.push(Row::single(
                    "W-04",
                    format!("{jobs} queued jobs, 64-byte payload"),
                    C::NAME,
                    w,
                    "RSS bytes per queued job",
                    "B",
                    rss,
                ));
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
                None => r.push(Row::missing(
                    "W-05",
                    v,
                    C::NAME,
                    w,
                    "context switches per second (not exposed on this OS)",
                )),
            }
        });
    }
}
