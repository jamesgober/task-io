//! fang job types, the shared run state the handlers write to, and queue
//! setup.
//!
//! fang 0.11 claims a task with a `SELECT` followed by an unconditional
//! `UPDATE ... SET state = 'in_progress'` (no transaction, no state check), so
//! two workers can run the same task. Every handler therefore records runs per
//! job id, and the suites count distinct ids, never raw completions.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use fang::asynk::async_queue::{AsyncQueue, AsyncQueueError, AsyncQueueable};
use fang::asynk::async_worker_pool::AsyncWorkerPool;
use fang::{AsyncRunnable, FangError, FangTaskState, SleepParams, Task, async_trait, typetag};
use serde::{Deserialize, Serialize};

/// What a job does when it runs.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum Mode {
    /// R-01: every tenth job fails its first attempt.
    FailFirst,
    /// R-02: every attempt fails while the outage lasts.
    Outage,
    /// P-02: append the id to the effects file.
    Effect,
}

#[derive(Serialize, Deserialize)]
pub struct FangJob {
    pub i: u64,
    pub payload: Vec<u8>,
    pub mode: Mode,
}

impl FangJob {
    pub fn new(i: u64, mode: Mode) -> Self {
        Self {
            i,
            payload: vec![(i & 0xff) as u8; 64],
            mode,
        }
    }
}

/// K-01: a job with fang's uniqueness on. fang hashes the serialized job, so
/// two submits with the same key are the same task to it.
#[derive(Serialize, Deserialize)]
pub struct FangKeyed {
    pub key: u64,
    pub payload: Vec<u8>,
}

/// R-02 attempt cap (first attempt plus nine retries), as for apalis.
pub const OUTAGE_ATTEMPTS: u32 = 10;

#[derive(Default)]
pub struct State {
    /// Successful runs per job id (or per key, for K-01).
    pub runs: HashMap<u64, u32>,
    /// Attempts per job id.
    pub attempts: HashMap<u64, u32>,
    /// R-02: jobs whose last allowed attempt failed.
    pub dead: HashSet<u64>,
    pub outage_until: Option<Instant>,
    pub attempts_during_outage: u64,
}

impl State {
    /// Jobs that either completed or used their last attempt.
    pub fn accounted(&self) -> u64 {
        let dead_only = self
            .dead
            .iter()
            .filter(|id| !self.runs.contains_key(id))
            .count();
        (self.runs.len() + dead_only) as u64
    }

    pub fn duplicate_runs(&self) -> u64 {
        self.runs
            .values()
            .map(|&c| u64::from(c.saturating_sub(1)))
            .sum()
    }
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
/// P-02: the effects file of a worker child.
pub static EFFECTS: OnceLock<Mutex<File>> = OnceLock::new();

pub fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn reset() {
    *state() = State::default();
}

fn fail(msg: &str) -> Result<(), FangError> {
    Err(FangError {
        description: msg.into(),
    })
}

#[typetag::serde]
#[async_trait]
impl AsyncRunnable for FangJob {
    async fn run(&self, _queue: &dyn AsyncQueueable) -> Result<(), FangError> {
        match self.mode {
            Mode::Effect => {
                if let Some(f) = EFFECTS.get()
                    && let Ok(mut f) = f.lock()
                {
                    let _ = f.write_all(&self.i.to_le_bytes());
                }
                Ok(())
            }
            Mode::FailFirst => {
                let mut s = state();
                let a = s.attempts.entry(self.i).or_insert(0);
                *a += 1;
                if self.i.is_multiple_of(10) && *a == 1 {
                    return fail("temporary failure (injected)");
                }
                *s.runs.entry(self.i).or_insert(0) += 1;
                Ok(())
            }
            Mode::Outage => {
                let mut s = state();
                let a = s.attempts.entry(self.i).or_insert(0);
                *a += 1;
                let a = *a;
                if s.outage_until.is_some_and(|t| Instant::now() < t) {
                    s.attempts_during_outage += 1;
                    if a >= OUTAGE_ATTEMPTS {
                        s.dead.insert(self.i);
                    }
                    return fail("downstream unavailable (injected)");
                }
                *s.runs.entry(self.i).or_insert(0) += 1;
                Ok(())
            }
        }
    }

    fn max_retries(&self) -> i32 {
        match self.mode {
            Mode::FailFirst => 3,
            Mode::Outage => OUTAGE_ATTEMPTS as i32 - 1,
            Mode::Effect => 0,
        }
    }

    // fang's backoff is in whole seconds. R-01 uses its minimum (1 s); R-02
    // uses 1 s then 2 s, the closest it can get to the 100 ms to 2 s
    // exponential backoff the apalis rows use.
    fn backoff(&self, attempt: u32) -> u32 {
        match self.mode {
            Mode::Outage if attempt > 0 => 2,
            _ => 1,
        }
    }
}

#[typetag::serde]
#[async_trait]
impl AsyncRunnable for FangKeyed {
    async fn run(&self, _queue: &dyn AsyncQueueable) -> Result<(), FangError> {
        *state().runs.entry(self.key).or_insert(0) += 1;
        Ok(())
    }

    fn uniq(&self) -> bool {
        true
    }
}

/// Opens `file` in `dir` at full durability (WAL, synchronous = FULL). With
/// `fresh`, any old copy is removed and fang's own schema (its
/// `sqlite_migrations/.../up.sql`) is applied through sqlx, so no second
/// SQLite library is linked.
pub async fn open_queue(dir: &Path, file: &str, fresh: bool, pool_size: u32) -> AsyncQueue {
    let _ = std::fs::create_dir_all(dir);
    if fresh {
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(dir.join(format!("{file}{ext}")));
        }
    }
    // fang parses this as a URL, where `sqlite://C:/...` would read `C:` as a
    // host; a relative URL from inside the directory avoids the drive letter.
    std::env::set_current_dir(dir).expect("enter db dir");
    let url = format!("sqlite:{file}?mode=rwc");
    let opts = sqlx::sqlite::SqliteConnectOptions::from_str(&url)
        .expect("sqlite url")
        .pragma("journal_mode", "WAL")
        .pragma("synchronous", "FULL");
    let pool = sqlx::SqlitePool::connect_with(opts)
        .await
        .expect("sqlite connect");
    if fresh {
        for stmt in include_str!("fang_schema.sql").split(';') {
            if !stmt.trim().is_empty() {
                sqlx::query(stmt).execute(&pool).await.expect("fang schema");
            }
        }
    }
    pool.close().await;
    let mut q = AsyncQueue::builder()
        .uri(url)
        .max_pool_size(pool_size)
        .build();
    q.connect().await.expect("fang connect");
    q
}

/// Rows in the task table, read with a separate connection.
pub async fn stored_rows(dir: &Path, file: &str) -> u64 {
    let url = format!(
        "sqlite://{}",
        dir.join(file).display().to_string().replace('\\', "/")
    );
    let Ok(opts) = sqlx::sqlite::SqliteConnectOptions::from_str(&url) else {
        return 0;
    };
    let Ok(pool) = sqlx::SqlitePool::connect_with(opts).await else {
        return 0;
    };
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fang_tasks")
        .fetch_one(&pool)
        .await
        .unwrap_or(0);
    pool.close().await;
    n.max(0) as u64
}

/// Starts fang's worker pool at its documented best for these workloads: 32
/// workers and a short poll sleep so polling does not throttle it.
pub async fn start_workers(q: &AsyncQueue) {
    let mut pool: AsyncWorkerPool<LoggedQueue> = AsyncWorkerPool::builder()
        .number_of_workers(32u32)
        .queue(LoggedQueue(q.clone()))
        .sleep_params(SleepParams {
            sleep_period: Duration::from_millis(10),
            max_sleep_period: Duration::from_millis(100),
            min_sleep_period: Duration::from_millis(10),
            sleep_step: Duration::from_millis(10),
        })
        .build();
    pool.start().await;
}

/// No progress for this long (far past fang's largest backoff here, 2 s)
/// means the remaining jobs are stuck.
pub const STALL: Duration = Duration::from_secs(60);

/// Waits until `done()` reaches `target`, `limit` passes, or `done()` has not
/// moved for `STALL`.
pub async fn wait_until(limit: Duration, target: u64, done: impl Fn() -> u64) -> bool {
    let t0 = Instant::now();
    let (mut last, mut moved) = (done(), Instant::now());
    while last < target {
        if t0.elapsed() > limit || moved.elapsed() > STALL {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
        let now = done();
        if now != last {
            (last, moved) = (now, Instant::now());
        }
    }
    true
}

/// Prints fang's errors (it reports worker failures only through `log`) and
/// counts them, so a stalled run says why. Warnings (sqlx's slow-statement
/// notices) are printed for the first few only and not counted.
struct StderrLog;

static LOGGED_ERRORS: LazyLock<Mutex<HashMap<String, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static WARNINGS: AtomicU64 = AtomicU64::new(0);

impl log::Log for StderrLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Warn
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        if r.level() == log::Level::Warn {
            if WARNINGS.fetch_add(1, Ordering::Relaxed) < 3 {
                eprintln!("  fang WARN: {}", r.args());
            }
            return;
        }
        let msg = format!("{}", r.args());
        if let Ok(mut m) = LOGGED_ERRORS.lock() {
            let n = m.entry(msg.clone()).or_insert(0);
            *n += 1;
            if *n == 1 {
                eprintln!("  fang {}: {msg}", r.level());
            }
        }
    }

    fn flush(&self) {}
}

pub fn init_log() {
    if log::set_logger(&StderrLog).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
}

/// Errors fang logged since the last call, then clears the tally.
pub fn take_logged_errors() -> u64 {
    LOGGED_ERRORS
        .lock()
        .map(|mut m| m.drain().map(|(_, n)| n).sum())
        .unwrap_or(0)
}

/// A fresh runtime per run: fang's worker pool has no stop, so dropping the
/// runtime is what ends a run's workers.
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .expect("rt")
}

/// fang's queue with every failed store call logged. fang's worker returns
/// such an error out of its loop, and its pool restarts a worker only when it
/// panics, so without this the worker leaves the pool without a trace.
#[derive(Clone)]
pub struct LoggedQueue(pub AsyncQueue);

/// Store calls a worker makes after running a job. An error from one of them
/// leaves the worker's loop for good (`run_tasks` returns it, and the pool's
/// supervisor restarts only workers that panicked).
const WORKER_ENDING: [&str; 4] = [
    "remove_task",
    "schedule_retry",
    "fail_task",
    "update_task_state",
];
static WORKER_EXITS: AtomicU64 = AtomicU64::new(0);

fn logged<T>(op: &str, r: Result<T, AsyncQueueError>) -> Result<T, AsyncQueueError> {
    if let Err(e) = &r {
        log::error!("{op} failed: {e:?}");
        if WORKER_ENDING.contains(&op) {
            WORKER_EXITS.fetch_add(1, Ordering::Relaxed);
        }
    }
    r
}

/// Workers that left the pool since the last call, then clears the count.
pub fn take_worker_exits() -> u64 {
    WORKER_EXITS.swap(0, Ordering::Relaxed)
}

#[async_trait]
impl AsyncQueueable for LoggedQueue {
    async fn fetch_and_touch_task(
        &self,
        task_type: Option<String>,
    ) -> Result<Option<Task>, AsyncQueueError> {
        logged(
            "fetch_and_touch_task",
            self.0.fetch_and_touch_task(task_type).await,
        )
    }

    async fn insert_task(&self, task: &dyn AsyncRunnable) -> Result<Task, AsyncQueueError> {
        logged("insert_task", self.0.insert_task(task).await)
    }

    async fn remove_all_tasks(&self) -> Result<u64, AsyncQueueError> {
        logged("remove_all_tasks", self.0.remove_all_tasks().await)
    }

    async fn remove_all_scheduled_tasks(&self) -> Result<u64, AsyncQueueError> {
        logged(
            "remove_all_scheduled_tasks",
            self.0.remove_all_scheduled_tasks().await,
        )
    }

    async fn remove_task(&self, id: &uuid::Uuid) -> Result<u64, AsyncQueueError> {
        logged("remove_task", self.0.remove_task(id).await)
    }

    async fn remove_task_by_metadata(
        &self,
        task: &dyn AsyncRunnable,
    ) -> Result<u64, AsyncQueueError> {
        logged(
            "remove_task_by_metadata",
            self.0.remove_task_by_metadata(task).await,
        )
    }

    async fn remove_tasks_type(&self, task_type: &str) -> Result<u64, AsyncQueueError> {
        logged(
            "remove_tasks_type",
            self.0.remove_tasks_type(task_type).await,
        )
    }

    async fn find_task_by_id(&self, id: &uuid::Uuid) -> Result<Task, AsyncQueueError> {
        logged("find_task_by_id", self.0.find_task_by_id(id).await)
    }

    async fn update_task_state(
        &self,
        task: &Task,
        state: FangTaskState,
    ) -> Result<Task, AsyncQueueError> {
        logged(
            "update_task_state",
            self.0.update_task_state(task, state).await,
        )
    }

    async fn fail_task(&self, task: &Task, error_message: &str) -> Result<Task, AsyncQueueError> {
        logged("fail_task", self.0.fail_task(task, error_message).await)
    }

    async fn schedule_task(&self, task: &dyn AsyncRunnable) -> Result<Task, AsyncQueueError> {
        logged("schedule_task", self.0.schedule_task(task).await)
    }

    async fn schedule_retry(
        &self,
        task: &Task,
        backoff_seconds: u32,
        error: &str,
    ) -> Result<Task, AsyncQueueError> {
        logged(
            "schedule_retry",
            self.0.schedule_retry(task, backoff_seconds, error).await,
        )
    }
}
