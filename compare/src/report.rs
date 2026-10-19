//! Collects result rows and writes them as markdown tables
//! (`results/<date>-<host>.md`) and raw CSV (`results/raw/`, not committed).

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

use crate::stats::{self, Summary};

pub enum Value {
    Summary {
        s: Summary,
        higher_is_better: bool,
    },
    Latency {
        p50: Summary,
        p99: Summary,
        p999: Summary,
    },
    Single(f64),
    Missing,
}

pub struct Row {
    pub suite: &'static str,
    pub variant: String,
    pub contender: &'static str,
    pub workers: usize,
    pub metric: String,
    pub unit: &'static str,
    pub value: Value,
    pub raw: Vec<f64>,
    /// Context, not a gated metric: shown, never ranked.
    pub info: bool,
}

impl Row {
    #[allow(clippy::too_many_arguments)]
    pub fn summary(
        suite: &'static str,
        variant: String,
        contender: &'static str,
        workers: usize,
        metric: &str,
        unit: &'static str,
        higher_is_better: bool,
        samples: &[f64],
    ) -> Self {
        Self {
            suite,
            variant,
            contender,
            workers,
            metric: metric.into(),
            unit,
            value: Value::Summary {
                s: stats::summarize(samples),
                higher_is_better,
            },
            raw: samples.to_vec(),
            info: false,
        }
    }

    pub fn latency(
        suite: &'static str,
        variant: String,
        contender: &'static str,
        workers: usize,
        runs: &[hdrhistogram::Histogram<u64>],
    ) -> Self {
        let q = |p: f64| {
            runs.iter()
                .map(|h| h.value_at_quantile(p) as f64)
                .collect::<Vec<_>>()
        };
        let (a, b, c) = (q(0.5), q(0.99), q(0.999));
        Self {
            suite,
            variant,
            contender,
            workers,
            metric: "submit-to-start latency".into(),
            unit: "ns",
            value: Value::Latency {
                p50: stats::summarize(&a),
                p99: stats::summarize(&b),
                p999: stats::summarize(&c),
            },
            raw: b,
            info: false,
        }
    }

    pub fn single(
        suite: &'static str,
        variant: String,
        contender: &'static str,
        workers: usize,
        metric: &str,
        unit: &'static str,
        v: f64,
    ) -> Self {
        Self {
            suite,
            variant,
            contender,
            workers,
            metric: metric.into(),
            unit,
            value: Value::Single(v),
            raw: vec![v],
            info: false,
        }
    }

    pub fn missing(
        suite: &'static str,
        variant: String,
        contender: &'static str,
        workers: usize,
        metric: &str,
    ) -> Self {
        Self {
            suite,
            variant,
            contender,
            workers,
            metric: metric.into(),
            unit: "",
            value: Value::Missing,
            raw: vec![],
            info: false,
        }
    }

    /// Marks this row as context (a count or a state), never ranked.
    pub fn info(mut self) -> Self {
        self.info = true;
        self
    }

    /// The number used to pick the best contender in a group; lower is better.
    fn rank_key(&self) -> Option<f64> {
        if self.info {
            return None;
        }
        match &self.value {
            Value::Summary {
                s,
                higher_is_better: true,
            } => Some(-s.median),
            Value::Summary {
                s,
                higher_is_better: false,
            } => Some(s.median),
            Value::Latency { p99, .. } => Some(p99.median),
            Value::Single(v) => Some(*v),
            Value::Missing => None,
        }
    }
}

pub struct Report {
    host: String,
    quick: bool,
    rows: Vec<Row>,
}

impl Report {
    pub fn new(host: &str, quick: bool) -> Self {
        Self {
            host: host.into(),
            quick,
            rows: Vec::new(),
        }
    }

    pub fn push(&mut self, row: Row) {
        eprintln!(
            "  {:<15} w={:<3} {} | {}",
            row.contender,
            row.workers,
            row.variant,
            fmt_value(&row)
        );
        self.rows.push(row);
    }

    pub fn finish(self) {
        let date = today();
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("results");
        let raw_dir = dir.join("raw");
        let _ = std::fs::create_dir_all(&raw_dir);
        let tag = if self.quick { "-quick" } else { "" };
        let md_path = dir.join(format!("{date}-{}{tag}.md", self.host));
        let csv_path = raw_dir.join(format!("{date}-{}{tag}.csv", self.host));

        let mut md = String::new();
        let new_file = !md_path.exists();
        if new_file {
            let _ = writeln!(md, "# Competitor results, {date}, host {}\n", self.host);
            let _ = writeln!(md, "- Host: {}", crate::sys::host_description());
            let _ = writeln!(
                md,
                "- Mode: {}",
                if self.quick {
                    "quick (smaller workloads; not for gating)"
                } else {
                    "full protocol"
                }
            );
            let _ = writeln!(
                md,
                "- Harness: `compare/` at the commit that added this file; contender versions in `compare/Cargo.toml`."
            );
            let _ = writeln!(
                md,
                "- **Best** marks the best contender in each group (the bar task-io must beat by 1.20).\n"
            );
        }
        let mut suites: Vec<&'static str> = self.rows.iter().map(|r| r.suite).collect();
        suites.dedup();
        for suite in suites {
            let _ = writeln!(md, "## {suite}\n");
            let _ = writeln!(
                md,
                "| Variant | Workers | Metric | Contender | Result | Best |"
            );
            let _ = writeln!(md, "|---|---|---|---|---|---|");
            let rows: Vec<&Row> = self.rows.iter().filter(|r| r.suite == suite).collect();
            for r in &rows {
                let group: Vec<&&Row> = rows
                    .iter()
                    .filter(|o| {
                        o.variant == r.variant && o.workers == r.workers && o.metric == r.metric
                    })
                    .collect();
                // Experiments (task-io prototypes) never set the bar; only
                // competitors can be Best.
                let best = group
                    .iter()
                    .filter(|o| !is_experiment(o.contender))
                    .filter_map(|o| o.rank_key().map(|k| (k, o.contender)))
                    .min_by(|a, b| a.0.total_cmp(&b.0))
                    .map(|(_, c)| c);
                let mark = if is_experiment(r.contender) {
                    "experiment"
                } else if r.info {
                    "context"
                } else if best == Some(r.contender) && group.len() > 1 {
                    "**Best**"
                } else {
                    ""
                };
                let _ = writeln!(
                    md,
                    "| {} | {} | {} | {} | {} | {} |",
                    r.variant,
                    r.workers,
                    r.metric,
                    r.contender,
                    fmt_value(r),
                    mark
                );
            }
            let _ = writeln!(md);
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&md_path)
            .expect("results file");
        let _ = f.write_all(md.as_bytes());

        let mut csv = String::new();
        if !csv_path.exists() {
            csv.push_str("suite,variant,contender,workers,metric,unit,samples\n");
        }
        for r in &self.rows {
            let samples: Vec<String> = r.raw.iter().map(|x| format!("{x}")).collect();
            let _ = writeln!(
                csv,
                "{},\"{}\",{},{},\"{}\",{},{}",
                r.suite,
                r.variant,
                r.contender,
                r.workers,
                r.metric,
                r.unit,
                samples.join(";")
            );
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&csv_path)
            .expect("raw file");
        let _ = f.write_all(csv.as_bytes());
        eprintln!("results: {}", md_path.display());
    }
}

fn fmt_value(r: &Row) -> String {
    // Four significant digits, so small values (microseconds shown in ms)
    // keep their precision.
    let n = |x: f64| {
        let a = x.abs();
        if a >= 1000.0 || a == 0.0 {
            format!("{x:.0}")
        } else if a >= 100.0 {
            format!("{x:.1}")
        } else if a >= 10.0 {
            format!("{x:.2}")
        } else if a >= 1.0 {
            format!("{x:.3}")
        } else {
            let digits = (3.0 - a.log10().floor()) as usize;
            format!("{x:.digits$}")
        }
    };
    match &r.value {
        Value::Summary { s, .. } => {
            format!(
                "{} {} (95% CI {} to {}, n={})",
                n(s.median),
                r.unit,
                n(s.lo),
                n(s.hi),
                s.n
            )
        }
        Value::Latency { p50, p99, p999 } => format!(
            "p50 {} / p99 {} / p99.9 {} ns (runs={})",
            n(p50.median),
            n(p99.median),
            n(p999.median),
            p99.n
        ),
        Value::Single(v) => format!("{} {}", n(*v), r.unit),
        Value::Missing => "not available".into(),
    }
}

/// Today's date (UTC) as YYYY-MM-DD.
fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// task-io prototypes and builds are measured but never set the bar.
pub fn is_experiment(contender: &str) -> bool {
    contender.starts_with("proto-") || contender.starts_with("task-io")
}
