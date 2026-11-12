//! Experiments I-11 and I-18 (dev/RESEARCH.md), and the P-01 half of I-28:
//! task-io's durable log, reduced to what P-01 measures. One segment file of
//! records in the format of docs/ARCHITECTURE.md section 8.2:
//!
//! ```text
//! | u32 length | u32 CRC-32C of (type .. payload) | u8 type | u64 job id | u64 token | payload |
//! ```
//!
//! (`length` counts the bytes from `type` to the end of the payload.)
//!
//! Group commit: a submit encodes its record and computes the checksum on
//! its own thread, appends the bytes to a shared buffer under a mutex, and
//! waits. One flusher thread swaps the buffer out, writes it, calls
//! `File::sync_data`, and then acknowledges every submit in that batch. A
//! lone submit is flushed at once; while a flush is in progress the next
//! batch fills, so batches grow with load and nothing waits on a timer.
//!
//! `prealloc` writes the segment full of zeros (and syncs it) before the
//! run, so later flushes overwrite allocated blocks and the file's size never
//! changes: `fdatasync` then has no metadata to write. In task-io a
//! background thread would prepare the next segment ahead of need; here it
//! is done before timing starts. A zero `length` ends the scan.
//!
//! `write_through` (with `prealloc` only) opens the segment for writing with
//! `FILE_FLAG_WRITE_THROUGH` on Windows or `O_DSYNC` on Linux and drops the
//! `sync_data` call: each write completes only once its data is on stable
//! storage (POSIX `open`, `O_DSYNC`: synchronized I/O data integrity
//! completion, the same as a `write` followed by `fdatasync`; Microsoft
//! Learn, "CreateFileW", caching behavior: write operations go directly to
//! disk, issued as forced-unit-access writes). The device then commits the
//! one write instead of flushing its whole cache. Whether a given device
//! honours that is what a power-loss test checks; this prototype measures
//! only the cost.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use crate::proto_crc;

pub const HEADER: usize = 8;
/// type, job id, token.
pub const FIXED: usize = 1 + 8 + 8;
/// Largest record recovery accepts; a longer `length` is damage (size-check
/// before allocating: a damaged length never makes the scan allocate).
pub const MAX_BODY: usize = 1 << 20;
pub const PUT: u8 = 1;

#[derive(Clone, Copy)]
pub struct Options {
    pub prealloc: Option<u64>,
    pub checksum: bool,
    pub write_through: bool,
}

fn open_write_through(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_WRITE_THROUGH (Microsoft Learn, "CreateFileW").
        o.custom_flags(0x8000_0000);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_DSYNC on Linux (asm-generic/fcntl.h: 00010000, octal).
        o.custom_flags(0o10000);
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "write-through is measured on Windows and Linux only",
        ));
    }
    #[allow(unreachable_code)]
    o.open(path)
}

struct State {
    buf: Vec<u8>,
    appended: u64,
    durable: u64,
    flushes: u64,
    stop: bool,
    failed: Option<io::ErrorKind>,
}

struct Shared {
    state: Mutex<State>,
    work: Condvar,
    done: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct Log {
    shared: Arc<Shared>,
    flusher: Option<JoinHandle<()>>,
    checksum: bool,
}

pub fn encode(out: &mut Vec<u8>, id: u64, payload: &[u8], checksum: bool) {
    let body = FIXED + payload.len();
    let start = out.len();
    out.extend_from_slice(&(body as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.push(PUT);
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(payload);
    if checksum {
        let crc = crate::proto_crc::crc32c_best(&out[start + HEADER..]);
        out[start + 4..start + 8].copy_from_slice(&crc.to_le_bytes());
    }
}

impl Log {
    pub fn create(path: &Path, opts: Options) -> io::Result<Self> {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        if let Some(len) = opts.prealloc {
            let zeros = vec![0u8; 1 << 20];
            let mut left = len;
            while left > 0 {
                let n = left.min(zeros.len() as u64) as usize;
                file.write_all(&zeros[..n])?;
                left -= n as u64;
            }
            file.sync_all()?;
            file.seek(SeekFrom::Start(0))?;
        }
        if opts.write_through {
            if opts.prealloc.is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "write-through is measured on a preallocated segment only",
                ));
            }
            drop(file);
            file = open_write_through(path)?;
        }
        let sync = !opts.write_through;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                buf: Vec::with_capacity(1 << 20),
                appended: 0,
                durable: 0,
                flushes: 0,
                stop: false,
                failed: None,
            }),
            work: Condvar::new(),
            done: Condvar::new(),
        });
        let s = shared.clone();
        let flusher = std::thread::Builder::new()
            .name("proto-log-flusher".into())
            .spawn(move || flush_loop(&s, file, sync))?;
        Ok(Self {
            shared,
            flusher: Some(flusher),
            checksum: opts.checksum,
        })
    }

    /// Appends a `Put` record and returns once it is on stable storage.
    pub fn submit(&self, id: u64, payload: &[u8]) -> io::Result<()> {
        // Encode and checksum off the lock.
        let mut rec = Vec::with_capacity(HEADER + FIXED + payload.len());
        encode(&mut rec, id, payload, self.checksum);
        let mut s = self.shared.lock();
        if let Some(e) = s.failed {
            return Err(e.into());
        }
        s.buf.extend_from_slice(&rec);
        s.appended += 1;
        let me = s.appended;
        self.shared.work.notify_one();
        while s.durable < me {
            if let Some(e) = s.failed {
                return Err(e.into());
            }
            s = self.shared.done.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        Ok(())
    }

    pub fn flushes(&self) -> u64 {
        self.shared.lock().flushes
    }
}

fn flush_loop(shared: &Shared, mut file: File, sync: bool) {
    let mut spare: Vec<u8> = Vec::with_capacity(1 << 20);
    loop {
        let end = {
            let mut s = shared.lock();
            while s.buf.is_empty() && !s.stop {
                s = shared.work.wait(s).unwrap_or_else(|e| e.into_inner());
            }
            if s.buf.is_empty() {
                return;
            }
            std::mem::swap(&mut s.buf, &mut spare);
            s.appended
        };
        let r = file
            .write_all(&spare)
            .and_then(|()| if sync { file.sync_data() } else { Ok(()) });
        spare.clear();
        let mut s = shared.lock();
        match r {
            Ok(()) => {
                s.durable = end;
                s.flushes += 1;
            }
            Err(e) => {
                s.failed = Some(e.kind());
            }
        }
        shared.done.notify_all();
        if s.failed.is_some() {
            return;
        }
    }
}

impl Drop for Log {
    fn drop(&mut self) {
        self.shared.lock().stop = true;
        self.shared.work.notify_all();
        if let Some(h) = self.flusher.take() {
            let _ = h.join();
        }
    }
}

/// What a recovery scan found.
#[derive(Debug, PartialEq, Eq)]
pub struct Recovered {
    pub ids: Vec<u64>,
    /// Bytes of valid records from the start of the segment.
    pub valid: u64,
    /// The scan stopped at a record that was cut short or failed its check
    /// (not at the zeros of a preallocated tail or the end of the file).
    pub damaged: bool,
}

/// Scans a segment from the start: every record whose length fits and whose
/// checksum matches, up to the first that does not (a torn tail).
pub fn recover(path: &Path, checksum: bool) -> io::Result<Recovered> {
    let mut data = Vec::new();
    File::open(path)?.read_to_end(&mut data)?;
    let mut at = 0usize;
    let mut ids = Vec::new();
    let mut damaged = false;
    while data.len() - at >= HEADER {
        let len = u32::from_le_bytes(data[at..at + 4].try_into().expect("4")) as usize;
        if len == 0 {
            break;
        }
        if !(FIXED..=MAX_BODY).contains(&len) || data.len() - at - HEADER < len {
            damaged = true;
            break;
        }
        let body = &data[at + HEADER..at + HEADER + len];
        let want = u32::from_le_bytes(data[at + 4..at + 8].try_into().expect("4"));
        if checksum && proto_crc::crc32c_best(body) != want {
            damaged = true;
            break;
        }
        ids.push(u64::from_le_bytes(body[1..9].try_into().expect("8")));
        at += HEADER + len;
    }
    if !damaged && data.len() - at > 0 && data.len() - at < HEADER {
        // A few bytes past the last record: zeros are fine, anything else
        // is a torn header.
        damaged = data[at..].iter().any(|&b| b != 0);
    }
    Ok(Recovered {
        ids,
        valid: at as u64,
        damaged,
    })
}

pub struct P01 {
    pub latency: hdrhistogram::Histogram<u64>,
    pub submits: u64,
    pub per_second: f64,
    pub flushes: u64,
}

/// P-01's shape: `target` durable submits of 64-byte payloads from
/// `submitters` threads, stopping at `time_box` (the proto is an experiment,
/// so the time box applies to it as to a competitor). Recovery is run after
/// and must find every acknowledged submit.
pub fn p01(
    dir: &Path,
    opts: Options,
    submitters: usize,
    target: u64,
    time_box: std::time::Duration,
) -> io::Result<P01> {
    use std::sync::atomic::{AtomicU64, Ordering};
    let path = dir.join("proto-log.seg");
    let mut opts = opts;
    if opts.prealloc.is_some() {
        opts.prealloc = Some(target * (HEADER + FIXED + 64) as u64);
    }
    let log = Log::create(&path, opts)?;
    let next = AtomicU64::new(0);
    let t0 = std::time::Instant::now();
    let lats: Vec<Vec<u64>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..submitters)
            .map(|_| {
                s.spawn(|| {
                    let mut lat = Vec::new();
                    while t0.elapsed() < time_box {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= target {
                            break;
                        }
                        let payload = [(i & 0xff) as u8; 64];
                        let t = std::time::Instant::now();
                        if log.submit(i, &payload).is_err() {
                            break;
                        }
                        lat.push(t.elapsed().as_nanos() as u64);
                    }
                    lat
                })
            })
            .collect();
        hs.into_iter()
            .map(|h| h.join().expect("submitter"))
            .collect()
    });
    let secs = t0.elapsed().as_secs_f64();
    let flushes = log.flushes();
    drop(log);
    let mut h = crate::stats::histogram();
    let mut n = 0u64;
    for lat in lats {
        n += lat.len() as u64;
        for v in lat {
            let _ = h.record(v.max(1));
        }
    }
    let rec = recover(&path, opts.checksum)?;
    assert!(!rec.damaged, "proto-log: damaged record after a clean run");
    assert_eq!(
        rec.ids.len() as u64,
        n,
        "proto-log: recovery found a different count than was acknowledged"
    );
    let _ = std::fs::remove_file(&path);
    Ok(P01 {
        latency: h,
        submits: n,
        per_second: n as f64 / secs,
        flushes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("proto-log-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn run(path: &Path, opts: Options, threads: u64, each: u64) {
        let log = Log::create(path, opts).unwrap();
        std::thread::scope(|s| {
            for t in 0..threads {
                let log = &log;
                s.spawn(move || {
                    for k in 0..each {
                        let id = t * each + k;
                        log.submit(id, &id.to_le_bytes().repeat(1 + (id % 9) as usize))
                            .unwrap();
                    }
                });
            }
        });
    }

    #[test]
    fn every_acknowledged_submit_recovers_once() {
        let d = dir("all");
        for prealloc in [None, Some(1 << 20)] {
            let p = d.join("a.seg");
            run(
                &p,
                Options {
                    prealloc,
                    checksum: true,
                    write_through: false,
                },
                4,
                500,
            );
            let r = recover(&p, true).unwrap();
            assert!(!r.damaged);
            let mut ids = r.ids;
            ids.sort_unstable();
            assert_eq!(ids, (0..2000).collect::<Vec<_>>());
        }
        #[cfg(any(windows, target_os = "linux"))]
        {
            let p = d.join("w.seg");
            let opts = Options {
                prealloc: Some(1 << 20),
                checksum: true,
                write_through: true,
            };
            run(&p, opts, 4, 200);
            let mut ids = recover(&p, true).unwrap().ids;
            ids.sort_unstable();
            assert_eq!(ids, (0..800).collect::<Vec<_>>());
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn torn_tail_and_flipped_bit_end_the_scan() {
        let d = dir("torn");
        let p = d.join("t.seg");
        run(
            &p,
            Options {
                prealloc: None,
                checksum: true,
                write_through: false,
            },
            1,
            100,
        );
        let full = std::fs::read(&p).unwrap();
        let all = recover(&p, true).unwrap();
        assert_eq!(all.ids.len(), 100);
        // Cut inside the last record.
        std::fs::write(&p, &full[..full.len() - 3]).unwrap();
        let r = recover(&p, true).unwrap();
        assert_eq!(r.ids.len(), 99);
        assert!(r.damaged);
        // Flip one payload bit in record 50: records 0..50 survive.
        let mut bad = full.clone();
        let mut at = 0usize;
        for _ in 0..50 {
            at += HEADER + u32::from_le_bytes(bad[at..at + 4].try_into().unwrap()) as usize;
        }
        bad[at + HEADER + FIXED] ^= 0x10;
        std::fs::write(&p, &bad).unwrap();
        let r = recover(&p, true).unwrap();
        assert_eq!(r.ids, (0..50).collect::<Vec<_>>());
        assert!(r.damaged);
        // A length past the limit is damage, not an allocation.
        let mut huge = full;
        huge[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&p, &huge).unwrap();
        let r = recover(&p, true).unwrap();
        assert!(r.ids.is_empty() && r.damaged);
        let _ = std::fs::remove_dir_all(&d);
    }
}
