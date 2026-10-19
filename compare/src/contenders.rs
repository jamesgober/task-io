//! The worker-pool contenders, each configured at its documented best for
//! running many independent closures on a fixed number of threads.
//!
//! Every contender takes the closure through its own generic API, so none of
//! them pays for an extra box the others do not. Each one also gets the
//! configuration a production user would give it (panics caught, no abort on a
//! panicking job), so the stress suites measure the contender, not a misuse.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// How long a contender may take to shut down (BENCHMARKS W-11).
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(10);

/// A pool that runs closures on `workers` threads.
pub trait Contender: Sync + Sized {
    const NAME: &'static str;
    fn start(workers: usize) -> Self;
    fn spawn<F: FnOnce() + Send + 'static>(&self, f: F);
    /// Submits without waiting; `false` when the contender refused the job
    /// because it is full. Unbounded contenders never refuse.
    fn try_spawn<F: FnOnce() + Send + 'static>(&self, f: F) -> bool {
        self.spawn(f);
        true
    }
    fn shutdown(self);
    /// Releases an adapter's bookkeeping for jobs that already finished (only
    /// affinitypool keeps any: its job handles). W-04 calls it after the
    /// warm-up batch, so that batch's handles are not freed inside the
    /// measured one.
    fn settle(&self) {}
    /// Bytes reserved up front per queue slot, for contenders that
    /// preallocate their queue (so W-04 counts that memory too).
    fn reserved_bytes_per_slot() -> Option<usize> {
        None
    }
}

type Thunk = Box<dyn FnOnce() + Send + 'static>;

fn run_worker(rx: crossbeam_channel::Receiver<Thunk>) {
    while let Ok(job) = rx.recv() {
        // A production pool must survive a panicking job.
        let _ = catch_unwind(AssertUnwindSafe(job));
    }
}

fn start_crossbeam(
    workers: usize,
    cap: Option<usize>,
) -> (crossbeam_channel::Sender<Thunk>, Vec<JoinHandle<()>>) {
    let (tx, rx) = match cap {
        Some(c) => crossbeam_channel::bounded::<Thunk>(c),
        None => crossbeam_channel::unbounded::<Thunk>(),
    };
    let threads = (0..workers)
        .map(|_| {
            let rx = rx.clone();
            std::thread::spawn(move || run_worker(rx))
        })
        .collect();
    (tx, threads)
}

// ---------------------------------------------------------------------------
// crossbeam-pool: std threads on an unbounded crossbeam MPMC channel. This is
// the pattern from the crossbeam-channel docs ("multiple consumers"), written
// as tight as we can write it, because it is what a user would hand-roll.
// ---------------------------------------------------------------------------

pub struct CrossbeamPool {
    tx: crossbeam_channel::Sender<Thunk>,
    threads: Vec<JoinHandle<()>>,
}

impl Contender for CrossbeamPool {
    const NAME: &'static str = "crossbeam-pool";

    fn start(workers: usize) -> Self {
        let (tx, threads) = start_crossbeam(workers, None);
        Self { tx, threads }
    }

    fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
        // An unbounded channel only fails when every receiver is gone, which
        // cannot happen before shutdown.
        let _ = self.tx.send(Box::new(f));
    }

    fn shutdown(self) {
        drop(self.tx);
        for t in self.threads {
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------------------
// crossbeam-bounded: the same pool on a bounded channel of 65,536 slots (the
// same capacity as a task-io lane by default). `spawn` waits when full;
// `try_spawn` refuses.
// ---------------------------------------------------------------------------

pub const BOUNDED_CAPACITY: usize = 65_536;

pub struct CrossbeamBounded {
    tx: crossbeam_channel::Sender<Thunk>,
    threads: Vec<JoinHandle<()>>,
}

impl Contender for CrossbeamBounded {
    const NAME: &'static str = "crossbeam-bounded";

    fn start(workers: usize) -> Self {
        let (tx, threads) = start_crossbeam(workers, Some(BOUNDED_CAPACITY));
        Self { tx, threads }
    }

    fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
        let _ = self.tx.send(Box::new(f));
    }

    fn try_spawn<F: FnOnce() + Send + 'static>(&self, f: F) -> bool {
        self.tx.try_send(Box::new(f)).is_ok()
    }

    fn shutdown(self) {
        drop(self.tx);
        for t in self.threads {
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------------------
// rayon: ThreadPool::spawn, the documented way to run detached closures on a
// sized pool (rayon docs, ThreadPool::spawn). A panic handler is installed,
// because without one a panic in a spawned job aborts the process
// (rayon docs, ThreadPoolBuilder::panic_handler).
// ---------------------------------------------------------------------------

pub struct Rayon(rayon::ThreadPool);

impl Contender for Rayon {
    const NAME: &'static str = "rayon";

    fn start(workers: usize) -> Self {
        Self(
            rayon::ThreadPoolBuilder::new()
                .num_threads(workers)
                .panic_handler(|_| {})
                .build()
                .expect("rayon pool"),
        )
    }

    fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
        self.0.spawn(f);
    }

    fn shutdown(self) {
        drop(self.0);
    }
}

// ---------------------------------------------------------------------------
// threadpool: ThreadPool::execute (threadpool docs). It replaces a thread
// whose job panicked on its own.
// ---------------------------------------------------------------------------

pub struct ThreadPoolCrate(threadpool::ThreadPool);

impl Contender for ThreadPoolCrate {
    const NAME: &'static str = "threadpool";

    fn start(workers: usize) -> Self {
        Self(threadpool::ThreadPool::new(workers))
    }

    fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
        self.0.execute(f);
    }

    fn shutdown(self) {
        self.0.join();
    }
}

// ---------------------------------------------------------------------------
// tokio-blocking: spawn_blocking on the multi-thread runtime, with the
// blocking pool capped at `workers` threads (tokio docs, Builder::
// max_blocking_threads). One async worker thread, since no async work runs.
// ---------------------------------------------------------------------------

fn tokio_shutdown(rt: Arc<tokio::runtime::Runtime>) {
    if let Ok(rt) = Arc::try_unwrap(rt) {
        rt.shutdown_timeout(SHUTDOWN_DEADLINE);
    }
}

pub struct TokioBlocking(Arc<tokio::runtime::Runtime>);

impl Contender for TokioBlocking {
    const NAME: &'static str = "tokio-blocking";

    fn start(workers: usize) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(workers)
            .enable_time()
            .build()
            .expect("tokio runtime");
        Self(Arc::new(rt))
    }

    fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
        drop(self.0.spawn_blocking(f));
    }

    fn shutdown(self) {
        tokio_shutdown(self.0);
    }
}

// ---------------------------------------------------------------------------
// tokio-blocking-elastic: spawn_blocking with tokio's default blocking pool,
// which grows on demand up to 512 threads and retires idle threads after 10 s
// (tokio docs, Builder::max_blocking_threads and thread_keep_alive). This is
// the elastic baseline for the load-step suite; `workers` is ignored.
// ---------------------------------------------------------------------------

pub struct TokioBlockingElastic(Arc<tokio::runtime::Runtime>);

impl Contender for TokioBlockingElastic {
    const NAME: &'static str = "tokio-blocking-elastic";

    fn start(_workers: usize) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .build()
            .expect("tokio runtime");
        Self(Arc::new(rt))
    }

    fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
        drop(self.0.spawn_blocking(f));
    }

    fn shutdown(self) {
        tokio_shutdown(self.0);
    }
}

// ---------------------------------------------------------------------------
// tokio-spawn: plain tasks on `workers` async worker threads. For short jobs
// that do not block, this is tokio's fastest way to run work, so it is added to
// raise the bar (BENCHMARKS section 3 allows adding competitors, never
// removing them).
// ---------------------------------------------------------------------------

pub struct TokioSpawn(Arc<tokio::runtime::Runtime>);

impl Contender for TokioSpawn {
    const NAME: &'static str = "tokio-spawn";

    fn start(workers: usize) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .enable_time()
            .build()
            .expect("tokio runtime");
        Self(Arc::new(rt))
    }

    fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
        drop(self.0.spawn(async move { f() }));
    }

    fn shutdown(self) {
        tokio_shutdown(self.0);
    }
}

/// Runs `$body` once per fixed-size contender type, binding the type to `$c`.
/// (The elastic tokio contender is used by name in the load-step suite only.)
#[macro_export]
macro_rules! for_each_contender {
    ($filter:expr, |$c:ident| $body:block) => {{
        {
            type $c = $crate::contenders::CrossbeamPool;
            if ($filter)(<$c as $crate::contenders::Contender>::NAME) $body
        }
        {
            type $c = $crate::contenders::CrossbeamBounded;
            if ($filter)(<$c as $crate::contenders::Contender>::NAME) $body
        }
        {
            type $c = $crate::contenders::Rayon;
            if ($filter)(<$c as $crate::contenders::Contender>::NAME) $body
        }
        {
            type $c = $crate::contenders::ThreadPoolCrate;
            if ($filter)(<$c as $crate::contenders::Contender>::NAME) $body
        }
        {
            type $c = $crate::contenders::TokioBlocking;
            if ($filter)(<$c as $crate::contenders::Contender>::NAME) $body
        }
        {
            type $c = $crate::contenders::TokioSpawn;
            if ($filter)(<$c as $crate::contenders::Contender>::NAME) $body
        }
        {
            type $c = $crate::survey::AffinityPool;
            if ($filter)(<$c as $crate::contenders::Contender>::NAME) $body
        }
        {
            type $c = $crate::survey::RustyPool;
            if ($filter)(<$c as $crate::contenders::Contender>::NAME) $body
        }
        {
            type $c = $crate::survey::ExecutorsWs;
            if ($filter)(<$c as $crate::contenders::Contender>::NAME) $body
        }
        {
            type $c = $crate::survey::KanalPool;
            if ($filter)(<$c as $crate::contenders::Contender>::NAME) $body
        }
        {
            type $c = $crate::survey::A3sLane;
            if ($filter)(<$c as $crate::contenders::Contender>::NAME) $body
        }
        {
            // Experiment (dev/RESEARCH.md I-01, I-04, I-16, I-27, I-36):
            // measured alongside the competitors, never counted as one.
            type $c = $crate::proto::ProtoRing;
            if ($filter)(<$c as $crate::contenders::Contender>::NAME) $body
        }
        {
            // Experiment (dev/RESEARCH.md I-35, I-03): proto-ring's pool on
            // per-submitter rings.
            type $c = $crate::proto::ProtoShards;
            if ($filter)(<$c as $crate::contenders::Contender>::NAME) $body
        }
    }};
}
