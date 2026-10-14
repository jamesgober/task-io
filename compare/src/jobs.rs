//! Job-system contenders. apalis's in-memory backend carries any `Send`
//! value and needs no serialization, so it runs the same closures as the
//! pools, through its own documented API (apalis README, "Usage"; apalis-core
//! `MemoryStorage`, `WorkerBuilder`, `ParallelizeExt`, `concurrency`).

use std::sync::{Arc, Mutex};

use apalis::prelude::*;
use apalis_core::backend::memory::{MemorySink, MemoryStorage};
use futures::{SinkExt, StreamExt};

use crate::contenders::{Contender, SHUTDOWN_DEADLINE};

type Thunk = Box<dyn FnOnce() + Send + 'static>;

/// The job argument: the closure, taken exactly once by the handler. The
/// mutex makes it `Sync`, which apalis requires of task arguments.
pub struct ClosureJob(Mutex<Option<Thunk>>);

async fn run_closure(job: ClosureJob) {
    let f = job.0.lock().ok().and_then(|mut g| g.take());
    if let Some(f) = f {
        f();
    }
}

/// apalis with its in-memory backend, a worker limited to `workers`
/// in-flight jobs, and jobs parallelized onto a tokio runtime with `workers`
/// threads.
pub struct ApalisMemory {
    rt: Arc<tokio::runtime::Runtime>,
    sink: MemorySink<ClosureJob>,
    push_lock: Mutex<MemorySink<ClosureJob>>,
    stop: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Contender for ApalisMemory {
    const NAME: &'static str = "apalis";

    fn start(workers: usize) -> Self {
        let rt = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(workers)
                .enable_all()
                .build()
                .expect("tokio runtime"),
        );
        let (tx, rx) = futures::channel::mpsc::unbounded::<Task<ClosureJob>>();
        let tx = Box::new(tx.sink_map_err(|e| e.into()))
            as Box<
                dyn futures::Sink<
                        Task<ClosureJob>,
                        Error = apalis_core::backend::memory::MemoryStorageError,
                    > + Send
                    + Sync
                    + Unpin,
            >;
        let sink = MemorySink::new(Arc::new(futures::lock::Mutex::new(tx)));
        let storage = MemoryStorage::new_with(sink.clone(), rx.boxed());
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        // The worker loop is driven by `block_on` on its own thread (its
        // future is not provably `Send` for every lifetime, which `spawn`
        // needs); `parallelize` still runs every job on the runtime's workers.
        let rt2 = rt.clone();
        let worker = std::thread::spawn(move || {
            rt2.block_on(async move {
                let w = WorkerBuilder::new("compare")
                    .backend(storage)
                    .concurrency(workers)
                    .parallelize(tokio::spawn)
                    .build(run_closure);
                let _ = w
                    .run_until(async move {
                        let _ = stop_rx.await;
                        Ok::<(), std::io::Error>(())
                    })
                    .await;
            });
        });
        Self {
            rt,
            push_lock: Mutex::new(sink.clone()),
            sink,
            stop: Mutex::new(Some(stop_tx)),
            worker: Mutex::new(Some(worker)),
        }
    }

    fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
        // apalis's push is async; from a plain thread it is driven to
        // completion with a minimal executor, as a sync caller would.
        //
        // apalis-core 1.0.0's `MemorySink::start_send` calls
        // `try_lock().unwrap()` on its shared sink, so two pushes at the same
        // moment panic inside apalis. Pushes are serialized here, the most
        // charitable workaround; the finding is recorded in the results.
        let job = ClosureJob(Mutex::new(Some(Box::new(f))));
        let mut sink = self.push_lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = futures::executor::block_on(sink.send(Task::new(job)));
    }

    fn shutdown(self) {
        if let Some(tx) = self.stop.lock().ok().and_then(|mut g| g.take()) {
            let _ = tx.send(());
        }
        if let Some(w) = self.worker.lock().ok().and_then(|mut g| g.take()) {
            let _ = w.join();
        }
        drop(self.sink);
        if let Ok(rt) = Arc::try_unwrap(self.rt) {
            rt.shutdown_timeout(SHUTDOWN_DEADLINE);
        }
    }
}
