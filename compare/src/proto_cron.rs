//! Experiment I-17 (dev/RESEARCH.md): a cron parser and a next-fire time
//! computed field by field from the parsed expression, never by scanning
//! minute by minute. Written to task-io's rules (no dependency, no `unsafe`,
//! no panics on any input, every loop bounded) so it can move into `src/time/`
//! after review; the tests below check it against a brute-force scan.
//!
//! Grammar: Vixie cron's five fields (minute, hour, day of month, month, day
//! of week), with lists (`1,5`), ranges (`1-5`), steps (`*/15`, `1-30/5`),
//! month and day names (`jan`, `mon`, any case), 0 or 7 for Sunday, and the
//! aliases `@yearly`, `@annually`, `@monthly`, `@weekly`, `@daily`,
//! `@midnight`, `@hourly`. Vixie's day rule: when both day fields are
//! restricted, a day matches if **either** matches; a field counts as
//! unrestricted when it starts with `*` (so `*/2` does too, as in Vixie).
//! Times are UTC with minute precision (PLAN 5.7: UTC by default).

/// Longest expression accepted, in bytes (hostile-input rule: refuse before
/// any work).
pub const MAX_LEN: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CronError {
    TooLong,
    NotAscii,
    FieldCount,
    Empty,
    BadNumber,
    OutOfRange,
    BadStep,
    BadRange,
    UnknownName,
    UnknownAlias,
    /// The expression parses but can never fire (for example `0 0 31 2 *`).
    NeverFires,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cron {
    minutes: u64, // bits 0..=59
    hours: u32,   // bits 0..=23
    dom: u32,     // bits 1..=31
    months: u16,  // bits 1..=12
    dow: u8,      // bits 0..=6, Sunday = 0
    dom_star: bool,
    dow_star: bool,
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

fn number(s: &str, names: &[&str], base: u32) -> Result<u32, CronError> {
    if s.is_empty() {
        return Err(CronError::Empty);
    }
    if s.bytes().all(|b| b.is_ascii_digit()) {
        // At most 2 digits are meaningful in any field; refusing longer ones
        // also rules out overflow.
        if s.len() > 2 {
            return Err(CronError::OutOfRange);
        }
        return s.parse().map_err(|_| CronError::BadNumber);
    }
    if names.is_empty() || s.len() != 3 {
        return Err(CronError::BadNumber);
    }
    let lower = s.to_ascii_lowercase();
    names
        .iter()
        .position(|n| *n == lower)
        .map(|i| i as u32 + base)
        .ok_or(CronError::UnknownName)
}

/// Parses one field into a bit set over `lo..=hi`.
fn field(s: &str, lo: u32, hi: u32, names: &[&str], name_base: u32) -> Result<u64, CronError> {
    if s.is_empty() {
        return Err(CronError::Empty);
    }
    let mut bits = 0u64;
    for item in s.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((r, st)) => {
                let st = number(st, &[], 0)?;
                if st == 0 || st > hi - lo + 1 {
                    return Err(CronError::BadStep);
                }
                (r, st)
            }
            None => (item, 1),
        };
        let (a, b) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (number(a, names, name_base)?, number(b, names, name_base)?)
        } else {
            let a = number(range, names, name_base)?;
            // `a/n` means from a to the top of the field (cronie's reading).
            (a, if step > 1 { hi } else { a })
        };
        if a < lo || b > hi {
            return Err(CronError::OutOfRange);
        }
        if a > b {
            return Err(CronError::BadRange);
        }
        let mut v = a;
        while v <= b {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok(bits)
}

impl Cron {
    pub fn parse(expr: &str) -> Result<Self, CronError> {
        if expr.len() > MAX_LEN {
            return Err(CronError::TooLong);
        }
        if !expr.is_ascii() {
            return Err(CronError::NotAscii);
        }
        let expr = expr.trim();
        let expr = match expr {
            "@yearly" | "@annually" => "0 0 1 1 *",
            "@monthly" => "0 0 1 * *",
            "@weekly" => "0 0 * * 0",
            "@daily" | "@midnight" => "0 0 * * *",
            "@hourly" => "0 * * * *",
            e if e.starts_with('@') => return Err(CronError::UnknownAlias),
            e => e,
        };
        let f: Vec<&str> = expr.split_ascii_whitespace().collect();
        if f.len() != 5 {
            return Err(CronError::FieldCount);
        }
        let dow7 = field(f[4], 0, 7, &DAYS, 0)?;
        let c = Cron {
            minutes: field(f[0], 0, 59, &[], 0)?,
            hours: field(f[1], 0, 23, &[], 0)? as u32,
            dom: field(f[2], 1, 31, &[], 0)? as u32,
            months: field(f[3], 1, 12, &MONTHS, 1)? as u16,
            // 7 is Sunday too.
            dow: ((dow7 & 0x7f) | ((dow7 >> 7) & 1)) as u8,
            dom_star: f[2].starts_with('*'),
            dow_star: f[4].starts_with('*'),
        };
        // With day of week unrestricted, the day rule is the day-of-month set
        // alone, which can be impossible in every chosen month.
        if c.dow_star && !c.dom_star {
            let first_dom = c.dom.trailing_zeros();
            let possible = (1..=12u32)
                .filter(|m| c.months & (1 << m) != 0)
                .any(|m| first_dom <= max_days(m));
            if !possible {
                return Err(CronError::NeverFires);
            }
        }
        Ok(c)
    }

    fn day_matches(&self, d: u32, weekday: u32) -> bool {
        let dom_ok = self.dom & (1 << d) != 0;
        let dow_ok = self.dow & (1 << weekday) != 0;
        if self.dom_star || self.dow_star {
            dom_ok && dow_ok
        } else {
            dom_ok || dow_ok
        }
    }

    /// The days of month `m` of year `y` that match the day rule, as bits
    /// `1..=days_in_month`; `wd1` is the weekday of the 1st.
    fn day_mask(&self, y: i64, m: u32, wd1: u32) -> u32 {
        let dim = days_in_month(y, m);
        let valid = ((1u64 << (dim + 1)) - 2) as u32;
        let mut by_weekday = 0u32;
        for j in 0..7 {
            if self.dow & (1 << ((wd1 + j) % 7)) != 0 {
                let mut k = j + 1;
                while k <= dim {
                    by_weekday |= 1 << k;
                    k += 7;
                }
            }
        }
        let days = if self.dom_star || self.dow_star {
            self.dom & by_weekday
        } else {
            self.dom | by_weekday
        };
        days & valid
    }

    /// Whether the minute starting at `t` (Unix seconds, UTC) matches.
    pub fn matches(&self, t: i64) -> bool {
        let days = t.div_euclid(86_400);
        let secs = t.rem_euclid(86_400);
        let (_, m, d) = civil_from_days(days);
        let h = (secs / 3600) as u32;
        let mi = ((secs % 3600) / 60) as u32;
        self.months & (1 << m) != 0
            && self.day_matches(d, weekday(days))
            && self.hours & (1 << h) != 0
            && self.minutes & (1 << mi) != 0
    }

    /// The first matching minute strictly after `after` (Unix seconds, UTC),
    /// computed field by field. `None` only if nothing matches within 400
    /// years (one Gregorian cycle), which `parse` already rules out.
    pub fn next_after(&self, after: i64) -> Option<i64> {
        let mut t = after.div_euclid(60).checked_add(1)?.checked_mul(60)?;
        let limit = t.checked_add(400 * 366 * 86_400)?;
        while t < limit {
            let days = t.div_euclid(86_400);
            let secs = t.rem_euclid(86_400);
            let (y, m, d) = civil_from_days(days);
            if self.months & (1 << m) == 0 {
                // First day of the next month in the set (maybe next year).
                let (ny, nm) = next_month(self.months, y, m);
                t = days_from_civil(ny, nm, 1) * 86_400;
                continue;
            }
            // The month's matching days as one mask, so a miss jumps straight
            // to the next matching day instead of stepping one day at a time.
            let mask = self.day_mask(y, m, weekday(days - i64::from(d - 1)));
            if mask & (1 << d) == 0 {
                match next_bit(u64::from(mask), d + 1, days_in_month(y, m)) {
                    Some(nd) => t = (days + i64::from(nd - d)) * 86_400,
                    None => {
                        let (ny, nm) = next_month(self.months, y, m);
                        t = days_from_civil(ny, nm, 1) * 86_400;
                    }
                }
                continue;
            }
            let h = (secs / 3600) as u32;
            let mi = ((secs % 3600) / 60) as u32;
            if self.hours & (1 << h) == 0 {
                match next_bit(self.hours as u64, h + 1, 23) {
                    Some(nh) => t = days * 86_400 + i64::from(nh) * 3600,
                    None => t = (days + 1) * 86_400,
                }
                continue;
            }
            if self.minutes & (1 << mi) == 0 {
                match next_bit(self.minutes, mi + 1, 59) {
                    Some(nmi) => t = days * 86_400 + i64::from(h) * 3600 + i64::from(nmi) * 60,
                    None => t = days * 86_400 + i64::from(h + 1) * 3600,
                }
                continue;
            }
            return Some(t);
        }
        None
    }
}

/// The lowest set bit in `from..=hi`.
fn next_bit(bits: u64, from: u32, hi: u32) -> Option<u32> {
    if from > hi {
        return None;
    }
    let masked = bits >> from << from;
    let b = masked.trailing_zeros();
    (b <= hi).then_some(b)
}

/// The next month in `set` after month `m` of year `y`.
fn next_month(set: u16, y: i64, m: u32) -> (i64, u32) {
    match next_bit(u64::from(set), m + 1, 12) {
        Some(nm) => (y, nm),
        None => (y + 1, (set as u64).trailing_zeros()),
    }
}

fn days_in_month(y: i64, m: u32) -> u32 {
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    if m == 2 && !leap { 28 } else { max_days(m) }
}

fn max_days(m: u32) -> u32 {
    match m {
        2 => 29,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// 0 = Sunday. 1970-01-01 was a Thursday.
fn weekday(days: i64) -> u32 {
    (days + 4).rem_euclid(7) as u32
}

/// Howard Hinnant's `days_from_civil`: days since 1970-01-01.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Expressions timed by the I-17 run. Each has one day field unrestricted,
/// so the cron crate (which may combine the two day fields differently)
/// computes the same schedule.
pub const TIMED: &[&str] = &[
    "0 3 * * *",
    "*/15 * * * *",
    "0 0 1 * *",
    "30 4 * * mon-fri",
    "0 9-17 * * mon-fri",
    "0 0 29 2 *",
];

/// Start times for the timing run: seeded, 1970 to 2099.
pub fn start_times(n: usize, seed: u64) -> Vec<i64> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s = s.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = s;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            ((z ^ (z >> 31)) % (130 * 365 * 86_400)) as i64
        })
        .collect()
}

/// The minute-by-minute scan that I-17 replaces (the baseline).
pub fn scan_after(c: &Cron, after: i64, max_minutes: i64) -> Option<i64> {
    let start = (after.div_euclid(60) + 1) * 60;
    (0..max_minutes)
        .map(|k| start + k * 60)
        .find(|&t| c.matches(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SplitMix64: a seeded generator, so a failing case can be replayed.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn at(y: i64, m: u32, d: u32, h: i64, mi: i64) -> i64 {
        days_from_civil(y, m, d) * 86_400 + h * 3600 + mi * 60
    }

    /// The oracle: scan minute by minute, up to `horizon` minutes.
    fn brute(c: &Cron, after: i64, horizon: i64) -> Option<i64> {
        let start = (after.div_euclid(60) + 1) * 60;
        (0..horizon).map(|k| start + k * 60).find(|&t| c.matches(t))
    }

    #[test]
    fn civil_round_trip() {
        for z in [-719_468i64, -1, 0, 1, 11_016, 19_000, 47_482, 2_932_896] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(weekday(0), 4);
        assert_eq!(civil_from_days(days_from_civil(2000, 2, 29)), (2000, 2, 29));
    }

    #[test]
    fn known_cases() {
        let t0 = at(2026, 1, 1, 0, 0); // a Thursday
        let cases: &[(&str, i64)] = &[
            ("0 3 * * *", at(2026, 1, 1, 3, 0)),
            ("*/15 * * * *", at(2026, 1, 1, 0, 15)),
            ("@hourly", at(2026, 1, 1, 1, 0)),
            ("0 0 29 2 *", at(2028, 2, 29, 0, 0)),
            ("0 0 31 * *", at(2026, 1, 31, 0, 0)),
            ("0 0 * * mon", at(2026, 1, 5, 0, 0)),
            ("0 0 * * 7", at(2026, 1, 4, 0, 0)),
            // Both day fields restricted: the 15th or a Friday, whichever is first.
            ("0 0 15 * fri", at(2026, 1, 2, 0, 0)),
            ("30 4 1 jan,jul *", at(2026, 1, 1, 4, 30)),
            ("5/20 * * * *", at(2026, 1, 1, 0, 5)),
        ];
        for (e, want) in cases {
            let c = Cron::parse(e).unwrap();
            assert_eq!(c.next_after(t0), Some(*want), "{e}");
        }
    }

    #[test]
    fn refuses_bad_input() {
        let bad: &[(&str, CronError)] = &[
            ("", CronError::FieldCount),
            ("* * * *", CronError::FieldCount),
            ("* * * * * *", CronError::FieldCount),
            ("60 * * * *", CronError::OutOfRange),
            ("* 24 * * *", CronError::OutOfRange),
            ("* * 0 * *", CronError::OutOfRange),
            ("* * * 13 *", CronError::OutOfRange),
            ("* * * * 8", CronError::OutOfRange),
            ("*/0 * * * *", CronError::BadStep),
            ("*/61 * * * *", CronError::BadStep),
            ("5-1 * * * *", CronError::BadRange),
            ("1- * * * *", CronError::Empty),
            ("1,,2 * * * *", CronError::Empty),
            ("99999999999999999999 * * * *", CronError::OutOfRange),
            ("* * * foo *", CronError::UnknownName),
            ("@reboot", CronError::UnknownAlias),
            ("0 0 31 2 *", CronError::NeverFires),
            ("0 0 30,31 2 *", CronError::NeverFires),
        ];
        for (e, want) in bad {
            assert_eq!(Cron::parse(e), Err(*want), "{e:?}");
        }
        assert_eq!(Cron::parse(&"1 ".repeat(200)), Err(CronError::TooLong));
        assert_eq!(Cron::parse("* * * * \u{e9}"), Err(CronError::NotAscii));
        // Vixie: with day of week restricted, day 31 of February is still
        // reachable through the weekday, so it is not refused.
        assert!(Cron::parse("0 0 31 2 mon").is_ok());
    }

    fn random_field(r: &mut Rng, lo: u64, hi: u64) -> String {
        match r.below(6) {
            0 => "*".into(),
            1 => format!("*/{}", 1 + r.below(hi - lo + 1)),
            2 => format!("{}", lo + r.below(hi - lo + 1)),
            3 => {
                let a = lo + r.below(hi - lo + 1);
                let b = a + r.below(hi - a + 1);
                format!("{a}-{b}")
            }
            4 => {
                let a = lo + r.below(hi - lo + 1);
                let b = a + r.below(hi - a + 1);
                format!("{a}-{b}/{}", 1 + r.below(hi - lo + 1))
            }
            _ => (0..1 + r.below(4))
                .map(|_| (lo + r.below(hi - lo + 1)).to_string())
                .collect::<Vec<_>>()
                .join(","),
        }
    }

    /// I-17's check: the field-by-field answer equals the minute-by-minute
    /// scan on random expressions and random start times.
    #[test]
    fn matches_brute_force() {
        let mut r = Rng(0x005e_ed17);
        let horizon = 9 * 366 * 24 * 60; // past the longest leap-year gap (8 years)
        let mut checked = 0;
        while checked < 1_500 {
            let e = format!(
                "{} {} {} {} {}",
                random_field(&mut r, 0, 59),
                random_field(&mut r, 0, 23),
                random_field(&mut r, 1, 31),
                random_field(&mut r, 1, 12),
                random_field(&mut r, 0, 7),
            );
            let Ok(c) = Cron::parse(&e) else { continue };
            // 1970 to 2099, any second.
            let after = r.below(130 * 365 * 86_400) as i64;
            let fast = c.next_after(after);
            let slow = brute(&c, after, horizon);
            match (fast, slow) {
                (Some(f), Some(s)) => assert_eq!(f, s, "{e:?} after {after}"),
                (Some(f), None) => {
                    assert!(
                        f > after + horizon * 60,
                        "{e:?} after {after}: brute found none"
                    )
                }
                (None, s) => panic!("{e:?} after {after}: fast found none, brute {s:?}"),
            }
            if let Some(f) = fast {
                assert!(c.matches(f) && f > after && f % 60 == 0, "{e:?}");
                assert!(c.next_after(f).is_some_and(|n| n > f), "{e:?}");
            }
            checked += 1;
        }
    }

    /// Hostile input: random strings from the cron alphabet never panic, and
    /// whatever parses has a next fire time.
    #[test]
    fn random_text_never_panics() {
        let alphabet = b"0123456789*/-, @jfmasondtwuhrbyiel\t";
        let mut r = Rng(0xf00d);
        for _ in 0..100_000 {
            let len = r.below(40) as usize;
            let s: String = (0..len)
                .map(|_| alphabet[r.below(alphabet.len() as u64) as usize] as char)
                .collect();
            if let Ok(c) = Cron::parse(&s) {
                assert!(c.next_after(0).is_some(), "{s:?}");
            }
        }
    }
}
