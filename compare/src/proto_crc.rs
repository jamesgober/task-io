//! Experiment I-28 (dev/RESEARCH.md): CRC-32C (Castagnoli, the iSCSI
//! polynomial, reflected 0x82F63B78) for durable records, written by hand so
//! task-io keeps zero dependencies (PLAN D-017; a checksum is not
//! cryptography, so it is ours to write).
//!
//! Three versions, each checked against the others and the crates:
//! - `portable`: slicing-by-8 over eight 256-entry tables built at compile
//!   time, for targets without the instruction (and Miri);
//! - `sse42`: one stream of the SSE 4.2 `crc32` instruction, 8 bytes at a
//!   time;
//! - `sse42_3way`: three independent streams over three equal lanes, then
//!   the lane results joined by shifting each register over the next lane's
//!   length. The instruction has a latency of 3 cycles and a throughput of
//!   one per cycle, so one stream leaves two thirds of it idle. A shift by a
//!   fixed length is linear in the register, so it is four table lookups
//!   (tables built at compile time from x^(8n) mod P). Mark Adler describes
//!   the method for crc32c (Stack Overflow, "Implementing SSE 4.2's CRC32C in
//!   software", 2013); the code here is written from the arithmetic, not
//!   copied.
//!
//! Every function works on the raw register (no inversion); `crc32c_*`
//! applies the standard pre- and post-inversion.

pub const POLY: u32 = 0x82F6_3B78;

/// `t[k][b]`: the register after byte `b` followed by `k` zero bytes, from a
/// zero register.
const fn slicing_tables() -> [[u32; 256]; 8] {
    let mut t = [[0u32; 256]; 8];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ POLY } else { c >> 1 };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut i = 0;
    while i < 256 {
        let mut s = 1;
        while s < 8 {
            let prev = t[s - 1][i];
            t[s][i] = (prev >> 8) ^ t[0][(prev & 0xff) as usize];
            s += 1;
        }
        i += 1;
    }
    t
}

static SLICING: [[u32; 256]; 8] = slicing_tables();

/// `a * b mod P`, reflected (bit 31 is x^0).
const fn multmodp(a: u32, mut b: u32) -> u32 {
    if a == 0 {
        return 0;
    }
    let mut m = 1u32 << 31;
    let mut p = 0u32;
    loop {
        if a & m != 0 {
            p ^= b;
            if a & (m - 1) == 0 {
                break;
            }
        }
        m >>= 1;
        b = if b & 1 != 0 { (b >> 1) ^ POLY } else { b >> 1 };
    }
    p
}

/// x^(8n) mod P: the operator that runs a register over `n` zero bytes.
const fn xpow8n(mut n: u64) -> u32 {
    let mut p = 1u32 << 31; // x^0
    let mut sq = 1u32 << 23; // x^8
    while n != 0 {
        if n & 1 != 0 {
            p = multmodp(sq, p);
        }
        sq = multmodp(sq, sq);
        n >>= 1;
    }
    p
}

/// A shift over a fixed number of zero bytes, as four byte tables.
pub struct Shift([[u32; 256]; 4]);

impl Shift {
    const fn new(len: u64) -> Self {
        let op = xpow8n(len);
        let mut t = [[0u32; 256]; 4];
        let mut k = 0;
        while k < 4 {
            let mut i = 0;
            while i < 256 {
                t[k][i] = multmodp(op, (i as u32) << (8 * k));
                i += 1;
            }
            k += 1;
        }
        Self(t)
    }

    #[inline(always)]
    fn apply(&self, c: u32) -> u32 {
        let t = &self.0;
        t[0][(c & 0xff) as usize]
            ^ t[1][((c >> 8) & 0xff) as usize]
            ^ t[2][((c >> 16) & 0xff) as usize]
            ^ t[3][(c >> 24) as usize]
    }
}

/// Lane lengths for the three-stream path: long lanes for large buffers,
/// short ones so records of a few hundred bytes still use three streams.
pub const LONG: usize = 4096;
pub const SHORT: usize = 128;
static SHIFT_LONG: Shift = Shift::new(LONG as u64);
static SHIFT_SHORT: Shift = Shift::new(SHORT as u64);

#[inline(always)]
fn le64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b.try_into().expect("8 bytes"))
}

pub fn portable_raw(mut crc: u32, data: &[u8]) -> u32 {
    let t = &SLICING;
    let mut chunks = data.chunks_exact(8);
    for c in &mut chunks {
        let lo = u32::from_le_bytes([c[0], c[1], c[2], c[3]]) ^ crc;
        let hi = u32::from_le_bytes([c[4], c[5], c[6], c[7]]);
        crc = t[7][(lo & 0xff) as usize]
            ^ t[6][((lo >> 8) & 0xff) as usize]
            ^ t[5][((lo >> 16) & 0xff) as usize]
            ^ t[4][(lo >> 24) as usize]
            ^ t[3][(hi & 0xff) as usize]
            ^ t[2][((hi >> 8) & 0xff) as usize]
            ^ t[1][((hi >> 16) & 0xff) as usize]
            ^ t[0][(hi >> 24) as usize];
    }
    for &b in chunks.remainder() {
        crc = (crc >> 8) ^ t[0][((crc ^ b as u32) & 0xff) as usize];
    }
    crc
}

pub fn crc32c_portable(data: &[u8]) -> u32 {
    !portable_raw(!0, data)
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::{LONG, SHIFT_LONG, SHIFT_SHORT, SHORT, Shift, le64};
    use std::arch::x86_64::{_mm_crc32_u8, _mm_crc32_u64};

    #[target_feature(enable = "sse4.2")]
    pub fn raw1(crc: u32, data: &[u8]) -> u32 {
        let mut c = crc as u64;
        let mut chunks = data.chunks_exact(8);
        for ch in &mut chunks {
            c = _mm_crc32_u64(c, le64(ch));
        }
        let mut crc = c as u32;
        for &b in chunks.remainder() {
            crc = _mm_crc32_u8(crc, b);
        }
        crc
    }

    /// Three streams over `block` (three lanes of `lane` bytes each, `lane`
    /// a multiple of 8), joined with `shift` (built for `lane` bytes).
    #[target_feature(enable = "sse4.2")]
    fn lanes3(crc: u32, block: &[u8], lane: usize, shift: &Shift) -> u32 {
        let (a, rest) = block.split_at(lane);
        let (b, c) = rest.split_at(lane);
        let (mut x, mut y, mut z) = (crc as u64, 0u64, 0u64);
        for ((pa, pb), pc) in a
            .chunks_exact(8)
            .zip(b.chunks_exact(8))
            .zip(c.chunks_exact(8))
        {
            x = _mm_crc32_u64(x, le64(pa));
            y = _mm_crc32_u64(y, le64(pb));
            z = _mm_crc32_u64(z, le64(pc));
        }
        // The register after lane a, run over lane b's length, joined with
        // lane b's register from zero; then the same for lane c.
        let ab = shift.apply(x as u32) ^ y as u32;
        shift.apply(ab) ^ z as u32
    }

    #[target_feature(enable = "sse4.2")]
    pub fn raw3(mut crc: u32, mut data: &[u8]) -> u32 {
        while data.len() >= 3 * LONG {
            let (block, rest) = data.split_at(3 * LONG);
            crc = lanes3(crc, block, LONG, &SHIFT_LONG);
            data = rest;
        }
        while data.len() >= 3 * SHORT {
            let (block, rest) = data.split_at(3 * SHORT);
            crc = lanes3(crc, block, SHORT, &SHIFT_SHORT);
            data = rest;
        }
        raw1(crc, data)
    }
}

pub fn has_sse42() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("sse4.2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// One stream of the instruction. Callers check `has_sse42()` first.
#[cfg(target_arch = "x86_64")]
pub fn crc32c_sse42(data: &[u8]) -> u32 {
    assert!(has_sse42());
    // SAFETY: SSE 4.2 is present (checked above).
    !unsafe { x86::raw1(!0, data) }
}

/// Three streams. Callers check `has_sse42()` first.
#[cfg(target_arch = "x86_64")]
pub fn crc32c_sse42_3way(data: &[u8]) -> u32 {
    assert!(has_sse42());
    // SAFETY: SSE 4.2 is present (checked above).
    !unsafe { x86::raw3(!0, data) }
}

/// How long each version takes per checksum of `size` bytes, over about
/// `total` bytes: `(name, ns per checksum)`. The buffer is random and stays
/// in cache for the sizes a record has (up to 64 KiB).
pub fn micro(size: usize, total: usize) -> Vec<(&'static str, f64)> {
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let buf: Vec<u8> = (0..size)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    let iters = (total / size.max(1)).max(1);
    let time = |f: &dyn Fn(&[u8]) -> u32| {
        let mut acc = 0u32;
        // Warm up the tables and the branch predictors.
        for _ in 0..iters.min(1_000) {
            acc ^= f(std::hint::black_box(&buf));
        }
        let t = std::time::Instant::now();
        for _ in 0..iters {
            acc ^= f(std::hint::black_box(&buf));
        }
        let ns = t.elapsed().as_nanos() as f64 / iters as f64;
        std::hint::black_box(acc);
        ns
    };
    let mut out: Vec<(&'static str, f64)> = Vec::new();
    out.push(("proto-portable", time(&crc32c_portable)));
    #[cfg(target_arch = "x86_64")]
    if has_sse42() {
        out.push(("proto-sse42", time(&crc32c_sse42)));
        out.push(("proto-sse42-3way", time(&crc32c_sse42_3way)));
    }
    out.push(("crc32c", time(&crc32c::crc32c)));
    out.push((
        "crc-fast",
        time(&|b: &[u8]| crc_fast::checksum(crc_fast::CrcAlgorithm::Crc32Iscsi, b) as u32),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    /// The check value of the catalogue (Greg Cook, "Catalogue of
    /// parametrised CRC algorithms", CRC-32/ISCSI: check 0xE3069283).
    #[test]
    fn check_value() {
        assert_eq!(crc32c_portable(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c_portable(b""), 0);
    }

    #[test]
    fn shift_matches_zero_bytes() {
        for len in [1u64, 7, 8, 64, SHORT as u64, LONG as u64] {
            let s = Shift::new(len);
            for c in [1u32, 0xdead_beef, 0x8000_0000, !0] {
                assert_eq!(
                    s.apply(c),
                    portable_raw(c, &vec![0; len as usize]),
                    "len {len}"
                );
            }
        }
    }

    /// Every version and both crates agree on every length from 0 to 1 KiB
    /// plus lengths around each lane boundary, at every alignment 0..8.
    #[test]
    fn versions_agree() {
        let buf = random(3 * 3 * LONG + 64, 0x00c0_ffee);
        let mut lens: Vec<usize> = (0..=1024).collect();
        for base in [3 * SHORT, 3 * LONG, 2 * 3 * LONG, 3 * LONG + 3 * SHORT] {
            lens.extend(base.saturating_sub(9)..base + 9);
        }
        for &len in &lens {
            for off in 0..8 {
                let d = &buf[off..off + len];
                let want = crc32c::crc32c(d);
                assert_eq!(crc32c_portable(d), want, "portable, len {len} off {off}");
                assert_eq!(
                    crc_fast::checksum(crc_fast::CrcAlgorithm::Crc32Iscsi, d) as u32,
                    want,
                    "crc-fast, len {len} off {off}"
                );
                #[cfg(target_arch = "x86_64")]
                if has_sse42() {
                    assert_eq!(crc32c_sse42(d), want, "sse42, len {len} off {off}");
                    assert_eq!(crc32c_sse42_3way(d), want, "3way, len {len} off {off}");
                }
            }
        }
    }

    /// A raw register carries over between calls, so a record can be
    /// checksummed in pieces (header, then payload).
    #[test]
    fn pieces_compose() {
        let buf = random(5000, 7);
        let want = crc32c::crc32c(&buf);
        for cut in [0, 1, 13, 384, 4999, 5000] {
            let (a, b) = buf.split_at(cut);
            assert_eq!(!portable_raw(portable_raw(!0, a), b), want);
            #[cfg(target_arch = "x86_64")]
            if has_sse42() {
                // SAFETY: SSE 4.2 is present.
                let r = unsafe { x86::raw3(x86::raw3(!0, a), b) };
                assert_eq!(!r, want);
            }
        }
    }
}
