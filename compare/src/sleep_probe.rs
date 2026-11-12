//! Experiment I-33 (dev/RESEARCH.md): how late a thread wakes from a timed
//! sleep, by primitive. D-01's lateness on Windows (about 7.5 ms at p50 for
//! every contender) matched the default timer granularity, not the
//! schedulers; this measures the sleep alone.
//!
//! Methods: `std::thread::park_timeout` (what the prototypes use),
//! `std::thread::sleep`, and on Windows a high-resolution waitable timer
//! (`CreateWaitableTimerExW` with `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`,
//! Windows 10 1803 and later; Microsoft Learn, "CreateWaitableTimerExW"),
//! alone and together with a wake event (`WaitForMultipleObjects`), which is
//! what a timer thread needs: a sleep that a submit can cut short.

use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub enum Method {
    ParkTimeout,
    Sleep,
    #[cfg(windows)]
    HiResTimer,
    #[cfg(windows)]
    HiResWakeable,
}

impl Method {
    pub fn all() -> Vec<(Method, &'static str)> {
        // Pushed to on Windows only.
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut v = vec![
            (Method::ParkTimeout, "park_timeout"),
            (Method::Sleep, "thread::sleep"),
        ];
        #[cfg(windows)]
        if win::HiResTimer::new().is_some() {
            v.push((Method::HiResTimer, "hi-res waitable timer"));
            v.push((Method::HiResWakeable, "hi-res timer + wake event"));
        }
        v
    }
}

/// `n` sleeps of targets spread uniformly over `min..max`; the histogram of
/// how late each woke, in ns (+1, so an on-time wake records).
pub fn lateness(
    method: Method,
    n: usize,
    min: Duration,
    max: Duration,
) -> hdrhistogram::Histogram<u64> {
    let mut h = crate::stats::histogram();
    #[cfg(windows)]
    let timer = match method {
        Method::HiResTimer => win::HiResTimer::new(),
        _ => None,
    };
    #[cfg(windows)]
    let wakeable = match method {
        Method::HiResWakeable => win::Wakeable::new(),
        _ => None,
    };
    let span = (max - min).as_nanos() as u64;
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let target = min + Duration::from_nanos(x % span.max(1));
        let t = Instant::now();
        match method {
            Method::ParkTimeout => {
                // park_timeout may return early (spurious or a stale token):
                // sleep again for what is left, as a timer thread would.
                while t.elapsed() < target {
                    std::thread::park_timeout(target - t.elapsed());
                }
            }
            Method::Sleep => std::thread::sleep(target),
            #[cfg(windows)]
            Method::HiResTimer => timer.as_ref().expect("checked in all()").wait(target),
            #[cfg(windows)]
            Method::HiResWakeable => wakeable
                .as_ref()
                .expect("checked in all()")
                .wait(Some(target)),
        }
        let late = t.elapsed().saturating_sub(target);
        let _ = h.record(late.as_nanos() as u64 + 1);
    }
    h
}

#[cfg(windows)]
pub use win::Wakeable;

#[cfg(windows)]
mod win {
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Threading::{
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateEventW, CreateWaitableTimerExW, INFINITE,
        SetEvent, SetWaitableTimer, TIMER_ALL_ACCESS, WaitForMultipleObjects, WaitForSingleObject,
    };

    pub struct HiResTimer(HANDLE);

    impl HiResTimer {
        pub fn new() -> Option<Self> {
            // SAFETY: null attributes and name are allowed; the flags and
            // access mask are the documented constants. A null handle means
            // failure (for example before Windows 10 1803).
            let h = unsafe {
                CreateWaitableTimerExW(
                    std::ptr::null(),
                    std::ptr::null(),
                    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                    TIMER_ALL_ACCESS,
                )
            };
            (!h.is_null()).then_some(Self(h))
        }

        pub fn wait(&self, d: Duration) {
            // A negative due time is relative, in 100 ns units.
            let due: i64 = -((d.as_nanos() / 100).max(1) as i64);
            // SAFETY: `self.0` is a live timer handle owned by `self`; `due`
            // outlives the call; no completion routine.
            let ok = unsafe { SetWaitableTimer(self.0, &due, 0, None, std::ptr::null(), 0) };
            if ok != 0 {
                // SAFETY: as above; the timer is signalled at the due time.
                unsafe { WaitForSingleObject(self.0, INFINITE) };
            } else {
                std::thread::sleep(d);
            }
        }
    }

    impl Drop for HiResTimer {
        fn drop(&mut self) {
            // SAFETY: the handle is owned by `self` and closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// A high-resolution timer and an auto-reset event: `wait` returns at
    /// the due time or when another thread calls `wake`, whichever is first.
    pub struct Wakeable {
        timer: HiResTimer,
        event: HANDLE,
    }

    // SAFETY: kernel handles may be used from any thread; the event is only
    // signalled (`SetEvent`) and waited on, both thread-safe.
    unsafe impl Send for Wakeable {}
    // SAFETY: as above.
    unsafe impl Sync for Wakeable {}

    impl Wakeable {
        pub fn new() -> Option<Self> {
            let timer = HiResTimer::new()?;
            // SAFETY: null attributes and name; auto-reset, initially unset.
            let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
            (!event.is_null()).then_some(Self { timer, event })
        }

        /// Sleeps for `d` (or until woken, with `None`). May return early;
        /// the caller checks the time again, as a timer thread does anyway.
        pub fn wait(&self, d: Option<Duration>) {
            let mut handles = [self.event, self.timer.0];
            let mut n = 1;
            if let Some(d) = d {
                let due: i64 = -((d.as_nanos() / 100).max(1) as i64);
                // SAFETY: a live timer handle owned by `self`; `due` outlives
                // the call.
                if unsafe { SetWaitableTimer(self.timer.0, &due, 0, None, std::ptr::null(), 0) }
                    != 0
                {
                    n = 2;
                } else {
                    handles[1] = self.event;
                }
            }
            // SAFETY: `handles[..n]` are live handles owned by `self`.
            unsafe { WaitForMultipleObjects(n, handles.as_ptr(), 0, INFINITE) };
        }

        pub fn wake(&self) {
            // SAFETY: a live event handle owned by `self`.
            unsafe { SetEvent(self.event) };
        }
    }

    impl Drop for Wakeable {
        fn drop(&mut self) {
            // SAFETY: owned by `self`, closed once (the timer closes itself).
            unsafe { CloseHandle(self.event) };
        }
    }
}
