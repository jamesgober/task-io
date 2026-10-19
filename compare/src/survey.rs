//! Worker-pool contenders added by the 2026-10-19 crates.io survey
//! (BENCHMARKS section 3), each at its documented best.
//!
//! Where a crate's API differs from the `Contender` contract, the adapter
//! below says what it does and what that costs:
//! - affinitypool has no fire-and-forget: `spawn` returns a future, and
//!   dropping it cancels a job that has not run. The adapter keeps every
//!   handle (one box each, the cost of fire-and-forget through a generic API)
//!   in per-thread shards and drops the ones that finished as it goes.
//! - rusty_pool and executors do not catch panics (a panic costs a thread
//!   restart), so jobs are wrapped in `catch_unwind`, as for crossbeam-pool.
//! - executors' `shutdown` drops queued jobs. That is the crate's behavior and
//!   W-11 records it; the adapter does not hide it.

use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Waker};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::contenders::Contender;

fn logical_cpus() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

// ---------------------------------------------------------------------------
// affinitypool: `Threadpool::new(workers)` with its default shards, as its own
// benchmarks use it.
// ---------------------------------------------------------------------------

type Handle = Pin<Box<dyn Future<Output = ()> + Send>>;

const HANDLE_SHARDS: usize = 64;
/// A shard drops its finished handles when it grows past this many.
const PRUNE_AT: usize = 4_096;

pub struct AffinityPool {
    pool: affinitypool::Threadpool,
    handles: Box<[Mutex<Vec<Handle>>]>,
}

fn handle_shard() -> usize {
    thread_local! {
        static SHARD: usize = {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            NEXT.fetch_add(1, Ordering::Relaxed) % HANDLE_SHARDS
        };
    }
    SHARD.with(|s| *s)
}

/// Drops every handle whose job has finished (polling a finished handle once
/// returns at once; jobs never panic, because the adapter catches panics).
fn prune(v: &mut Vec<Handle>) {
    let mut cx = Context::from_waker(Waker::noop());
    v.retain_mut(|h| h.as_mut().poll(&mut cx).is_pending());
}

impl Contender for AffinityPool {
    const NAME: &'static str = "affinitypool";

    fn start(workers: usize) -> Self {
        Self {
            pool: affinitypool::Threadpool::new(workers),
            handles: (0..HANDLE_SHARDS).map(|_| Mutex::new(Vec::new())).collect(),
        }
    }

    fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
        let h: Handle = Box::pin(self.pool.spawn(move || {
            let _ = catch_unwind(AssertUnwindSafe(f));
        }));
        if let Ok(mut v) = self.handles[handle_shard()].lock() {
            v.push(h);
            if v.len() >= PRUNE_AT && v.len().is_power_of_two() {
                prune(&mut v);
            }
        }
    }

    fn settle(&self) {
        for shard in &self.handles {
            if let Ok(mut v) = shard.lock() {
                prune(&mut v);
            }
        }
    }

    fn shutdown(self) {
        // Dropping the pool runs every queued job and joins its threads; the
        // handles are dropped after, when nothing is left to cancel.
        let Self { pool, handles } = self;
        drop(pool);
        drop(handles);
    }
}

// ---------------------------------------------------------------------------
// rusty_pool: a fixed pool (core = max = workers, threads started up front),
// and the elastic configuration from its docs' defaults (max twice the
// logical CPUs, 60 s keep-alive) for W-12 and W-13.
// ---------------------------------------------------------------------------

struct Exits {
    n: Mutex<usize>,
    cv: Condvar,
}

fn rusty(core: usize, max: usize, keep_alive: Duration) -> (rusty_pool::ThreadPool, Arc<Exits>) {
    let exits = Arc::new(Exits {
        n: Mutex::new(0),
        cv: Condvar::new(),
    });
    let e = exits.clone();
    let pool = rusty_pool::Builder::new()
        .core_size(core)
        .max_size(max)
        .keep_alive(keep_alive)
        .on_worker_stop(move || {
            if let Ok(mut n) = e.n.lock() {
                *n += 1;
                e.cv.notify_all();
            }
        })
        .build();
    pool.start_core_threads();
    (pool, exits)
}

/// `shutdown_join` runs every queued job and waits for the workers to go
/// idle, but does not join their threads; the stop callback counts exits.
fn rusty_shutdown(pool: rusty_pool::ThreadPool, exits: &Exits) {
    let started = pool.get_current_worker_count();
    pool.shutdown_join();
    if let Ok(mut n) = exits.n.lock() {
        while *n < started {
            match exits.cv.wait_timeout(n, Duration::from_secs(10)) {
                Ok((g, t)) if !t.timed_out() => n = g,
                _ => return,
            }
        }
    }
}

pub struct RustyPool(rusty_pool::ThreadPool, Arc<Exits>);

impl Contender for RustyPool {
    const NAME: &'static str = "rusty-pool";

    fn start(workers: usize) -> Self {
        let (p, e) = rusty(workers, workers, Duration::from_secs(60));
        Self(p, e)
    }

    fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
        self.0.execute(move || {
            let _ = catch_unwind(AssertUnwindSafe(f));
        });
    }

    fn shutdown(self) {
        rusty_shutdown(self.0, &self.1);
    }
}

pub struct RustyPoolElastic(rusty_pool::ThreadPool, Arc<Exits>);

impl Contender for RustyPoolElastic {
    const NAME: &'static str = "rusty-pool-elastic";

    /// `workers` is the core size (0: the logical CPU count, its default).
    fn start(workers: usize) -> Self {
        let core = if workers == 0 {
            logical_cpus()
        } else {
            workers
        };
        let max = (2 * logical_cpus()).max(core);
        let (p, e) = rusty(core, max, Duration::from_secs(60));
        Self(p, e)
    }

    fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
        self.0.execute(move || {
            let _ = catch_unwind(AssertUnwindSafe(f));
        });
    }

    fn shutdown(self) {
        rusty_shutdown(self.0, &self.1);
    }
}

// ---------------------------------------------------------------------------
// executors: `crossbeam_workstealing_pool::small_pool`, the pool its README
// shows fastest for throughput (default features). Built with the harness's
// `ws-no-park` feature, workers never park (its lowest-latency mode) and the
// rows are named for it.
// ---------------------------------------------------------------------------

pub struct ExecutorsWs(
    executors::crossbeam_workstealing_pool::ThreadPool<
        executors::parker::StaticParker<executors::parker::SmallThreadData>,
    >,
);

impl Contender for ExecutorsWs {
    const NAME: &'static str = if cfg!(feature = "ws-no-park") {
        "executors-ws-no-park"
    } else {
        "executors-ws"
    };

    fn start(workers: usize) -> Self {
        assert!(workers <= 32, "small_pool supports at most 32 threads");
        Self(executors::crossbeam_workstealing_pool::small_pool(workers))
    }

    fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
        use executors::Executor;
        self.0.execute(move || {
            let _ = catch_unwind(AssertUnwindSafe(f));
        });
    }

    fn shutdown(self) {
        use executors::Executor;
        let _ = self.0.shutdown();
    }
}

// ---------------------------------------------------------------------------
// a3s-lane: one lane with `workers` concurrency, built with its core queue
// only. Its commands are async, so a closure (which may block) runs inside
// `block_in_place`, which hands the worker's other tasks (a3s-lane's scheduler
// among them) to another thread first. Run plainly inline, a blocked job
// stalls the scheduler with it (W-04 deadlocked that way); `block_in_place`
// was faster than `spawn_blocking` (W-01 at 4 workers, 1 submitter: 6.1 s
// against 17.4 s, quick runs). `Command` takes `&self`, so the
// closure sits in a `Mutex<Option<_>>` and runs once; the result receiver is
// dropped (the job still runs). Submitting is async, so a sync caller blocks
// on it.
// ---------------------------------------------------------------------------

struct LaneJob<F>(Mutex<Option<F>>);

#[async_trait::async_trait]
impl<F: FnOnce() + Send + 'static> a3s_lane::Command for LaneJob<F> {
    async fn execute(&self) -> a3s_lane::Result<serde_json::Value> {
        let f = self.0.lock().ok().and_then(|mut g| g.take());
        if let Some(f) = f {
            tokio::task::block_in_place(move || {
                let _ = catch_unwind(AssertUnwindSafe(f));
            });
        }
        Ok(serde_json::Value::Null)
    }

    fn command_type(&self) -> &str {
        "bench"
    }
}

pub struct A3sLane {
    rt: tokio::runtime::Runtime,
    manager: a3s_lane::QueueManager,
}

impl Contender for A3sLane {
    const NAME: &'static str = "a3s-lane";

    fn start(workers: usize) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers.max(1))
            .max_blocking_threads(workers.max(1))
            .enable_all()
            .build()
            .expect("tokio runtime");
        let manager = rt.block_on(async {
            let m = a3s_lane::QueueManagerBuilder::new(a3s_lane::EventEmitter::new(100))
                .with_lane("work", a3s_lane::LaneConfig::new(workers, workers), 0)
                .build()
                .await
                .expect("a3s-lane manager");
            m.start().await.expect("a3s-lane start");
            m
        });
        Self { rt, manager }
    }

    fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
        let job = Box::new(LaneJob(Mutex::new(Some(f))));
        let _ = self.rt.block_on(self.manager.submit("work", job));
    }

    fn shutdown(self) {
        self.rt.block_on(async {
            self.manager.shutdown().await;
            let _ = self.manager.drain(Duration::from_secs(10)).await;
        });
    }
}

// ---------------------------------------------------------------------------
// kanal-pool: crossbeam-pool's design (std threads looping on `recv`) on an
// unbounded kanal channel. Dropping the last sender lets the workers drain
// the queue and stop; `close()` would discard queued jobs, so it is not used.
// ---------------------------------------------------------------------------

type Thunk = Box<dyn FnOnce() + Send + 'static>;

pub struct KanalPool {
    tx: kanal::Sender<Thunk>,
    threads: Vec<JoinHandle<()>>,
}

impl Contender for KanalPool {
    const NAME: &'static str = "kanal-pool";

    fn start(workers: usize) -> Self {
        let (tx, rx) = kanal::unbounded::<Thunk>();
        let threads = (0..workers)
            .map(|_| {
                let rx = rx.clone();
                std::thread::spawn(move || {
                    while let Ok(job) = rx.recv() {
                        let _ = catch_unwind(AssertUnwindSafe(job));
                    }
                })
            })
            .collect();
        Self { tx, threads }
    }

    fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
        let _ = self.tx.send(Box::new(f));
    }

    fn shutdown(self) {
        drop(self.tx);
        for t in self.threads {
            let _ = t.join();
        }
    }
}
