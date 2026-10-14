//! The worker-pool contenders, each configured at its documented best for
//! running many independent closures on a fixed number of threads.
//!
//! Every contender takes the closure through its own generic API, so none of
//! them pays for an extra box the others do not.

use std::sync::Arc;
use std::thread::JoinHandle;

/// A pool that runs closures on `workers` threads.
pub trait Contender: Sync + Sized {
    const NAME: &'static str;
    fn start(workers: usize) -> Self;
    fn spawn<F: FnOnce() + Send + 'static>(&self, f: F);
    fn shutdown(self);
}

// ---------------------------------------------------------------------------
// crossbeam-pool: std threads on a crossbeam MPMC channel. This is the pattern
// from the crossbeam-channel docs ("multiple consumers"), written as tight as
// we can write it, because it is what a user would hand-roll.
// ---------------------------------------------------------------------------

type Thunk = Box<dyn FnOnce() + Send + 'static>;

pub struct CrossbeamPool {
    tx: crossbeam_channel::Sender<Thunk>,
    threads: Vec<JoinHandle<()>>,
}

impl Contender for CrossbeamPool {
    const NAME: &'static str = "crossbeam-pool";

    fn start(workers: usize) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<Thunk>();
        let threads = (0..workers)
            .map(|_| {
                let rx = rx.clone();
                std::thread::spawn(move || {
                    while let Ok(job) = rx.recv() {
                        job();
                    }
                })
            })
            .collect();
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
// rayon: ThreadPool::spawn, the documented way to run detached closures on a
// sized pool (rayon docs, ThreadPool::spawn).
// ---------------------------------------------------------------------------

pub struct Rayon(rayon::ThreadPool);

impl Contender for Rayon {
    const NAME: &'static str = "rayon";

    fn start(workers: usize) -> Self {
        Self(
            rayon::ThreadPoolBuilder::new()
                .num_threads(workers)
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
// threadpool: ThreadPool::execute (threadpool docs).
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
        if let Ok(rt) = Arc::try_unwrap(self.0) {
            rt.shutdown_timeout(std::time::Duration::from_secs(30));
        }
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
        if let Ok(rt) = Arc::try_unwrap(self.0) {
            rt.shutdown_timeout(std::time::Duration::from_secs(30));
        }
    }
}

/// Runs `$body` once per contender type, binding the type to `$c`.
#[macro_export]
macro_rules! for_each_contender {
    ($filter:expr, |$c:ident| $body:block) => {{
        {
            type $c = $crate::contenders::CrossbeamPool;
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
    }};
}
