//! Statistics: medians with bootstrap confidence intervals, and latency
//! histograms.

/// Median and a 95% bootstrap confidence interval of the median.
#[derive(Clone, Copy, Debug)]
pub struct Summary {
    pub median: f64,
    pub lo: f64,
    pub hi: f64,
    pub n: usize,
}

pub fn summarize(samples: &[f64]) -> Summary {
    assert!(!samples.is_empty(), "no samples");
    let mid = median(samples);
    if samples.len() < 3 {
        return Summary {
            median: mid,
            lo: mid,
            hi: mid,
            n: samples.len(),
        };
    }
    // 2,000 bootstrap resamples with a fixed seed, so a table is reproducible
    // from its raw samples.
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    let mut medians = Vec::with_capacity(2000);
    let mut buf = vec![0.0; samples.len()];
    for _ in 0..2000 {
        for slot in buf.iter_mut() {
            *slot = samples[(rng.next() % samples.len() as u64) as usize];
        }
        medians.push(median(&buf));
    }
    medians.sort_by(|a, b| a.total_cmp(b));
    let lo = medians[(medians.len() as f64 * 0.025) as usize];
    let hi = medians[((medians.len() as f64 * 0.975) as usize).min(medians.len() - 1)];
    Summary {
        median: mid,
        lo,
        hi,
        n: samples.len(),
    }
}

fn median(xs: &[f64]) -> f64 {
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let m = v.len() / 2;
    if v.len() % 2 == 0 {
        (v[m - 1] + v[m]) / 2.0
    } else {
        v[m]
    }
}

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// A latency histogram in nanoseconds, 1 ns to 100 s, 3 significant digits.
pub fn histogram() -> hdrhistogram::Histogram<u64> {
    hdrhistogram::Histogram::new_with_bounds(1, 100_000_000_000, 3).expect("histogram bounds")
}
