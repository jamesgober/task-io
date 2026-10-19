# task-io - Architecture

> How task-io is built inside. **Status: draft for the `0.2.0` design lock.** Sections marked *(pending I-xx)* wait on the experiment named in [`../dev/RESEARCH.md`](../dev/RESEARCH.md); the final version is written from the measured design (ROADMAP v0.2.0, step 4). The public surface is in [`API.md`](./API.md).

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>

## Contents

1. [Threads](#1-threads)
2. [The job path](#2-the-job-path)
3. [Job slots and lanes](#3-job-slots-and-lanes)
4. [Workers and parking](#4-workers-and-parking)
5. [The controller](#5-the-controller)
6. [Time: delays and cron](#6-time-delays-and-cron)
7. [Failures, retries, leases](#7-failures-retries-leases)
8. [The store and the durable log](#8-the-store-and-the-durable-log)
9. [Shutdown](#9-shutdown)
10. [Isolated workers](#10-isolated-workers)
11. [The `unsafe` map](#11-the-unsafe-map)
12. [The `sys/` surface](#12-the-sys-surface)

---

## 1. Threads

| Thread | Count | Job |
|---|---|---|
| workers | floor to cap (default start: physical cores) | run handlers |
| controller | 1 | read health and load, scale, repair; parked when idle |
| timer | 1, only while delayed jobs or schedules exist | move due jobs to their lanes |
| durable flusher | 1, only with the durable store | group-commit appends and flush to stable storage |
| isolated children | 0 until an isolated handler is registered | separate processes, not threads |

When the pool is idle, no thread wakes: workers, the controller, and the timer all park with no timeout when there is nothing to run, watch, or wait for (W-05: zero wakeups).

## 2. The job path

```
submit(name, payload)
  -> admission: handler registered? payload <= limit? key not in dedupe window? tenant quota?
                expected wait within the job's deadline? (I-29)
  -> store.put(job)                     memory: a slot write; durable: an append, acked after flush
  -> delayed?  timer wheel : lane ring
  -> worker takes a batch from a lane (DRR across lanes)            (I-02, I-08)
  -> lease(job, token)  -> run handler (catch_unwind)
  -> Ok         : store.complete(id, token)
     Fail::retry: store.retry(id, token, at = now + backoff)  -> timer wheel
     permanent  : store.dead_letter(id, token, reasons)
```

Every step is bounded. A token mismatch at `complete`, `retry`, or `dead_letter` means the lease moved (repair or failover) and the result is dropped, with an event.

## 3. Job slots and lanes

- A job lives in a fixed **slot**: a 16-byte header (id, attempt, lane, flags) and 64 bytes of inline payload; larger payloads are stored out of line and the slot holds a pointer. 80 bytes in all *(pending I-36)*.
- Each lane is a bounded ring of slot indexes (a Vyukov MPMC ring), cache-line padded head and tail *(pending I-01, I-03)*.
- Submitters do not all write one ring tail: the lane's slots are split into per-submitter rings that workers drain in place, so no job waits on a flush. The quick `proto-shards` pass ran 32 submitters 6.1x faster than crossbeam-pool; the shard count and the ordering cost are still to be measured *(pending I-35, RESEARCH section 6)*.
- With one lane (the default), workers read it directly: no DRR state, no branch on the hot path (`LIBRARIES.md` 4.3). With several, DRR picks the next lane by weight and deficit; aging lifts a job that waited past its bound.

## 4. Workers and parking

- A worker loops: take a batch from its lane choice, run each job, write its health record (plain relaxed stores, its own cache line), repeat.
- With nothing to do, it announces itself idle (one bit in an idle mask), re-checks the lanes, then parks on its own word through `futex` / `WaitOnAddress` / `os_sync_wait_on_address` *(pending I-04, I-34)*. A submitter that sees idle bits after publishing wakes exactly one worker. The announce, re-check, and wake are ordered with sequentially consistent fences, so no wakeup is lost (loom model required).
- Placement: workers are pinned or hinted to core complexes so a lane's workers share an L3 when the load fits *(pending I-05, I-06)*.

## 5. The controller

### 5.1 Inputs, sampled every interval while jobs run *(interval pending I-21)*

| Input | From |
|---|---|
| per worker: state, current job start, jobs done, failures, panics, busy time | the worker's health record |
| per worker: CPU time | `sys/` per-thread CPU clock |
| per lane: depth, oldest ready age, arrivals, completions | lane counters |
| per handler: failure rate, mean and spread of job time | aggregated from health records |

### 5.2 Worker states and actions

```
           +---------+   job time >> peers (MAD)     +------+
 start --> | healthy | ----------------------------> | slow | --stays slow--> retire + replace
           +---------+ <---------- recovers --------- +------+
              |   \
  job past    |    \  failure rate high, follows the worker
  lease/deadline    \------------------------------> retire + replace
              v
           +-------+  replacement started at once; the stuck thread takes no new job,
           | stuck |  its late result is fenced off; it counts against the zombie cap
           +-------+
 heartbeat stops / thread ended outside shutdown -> dead: re-queue its leased jobs, replace, critical event
 failures follow a handler, not a worker        -> open that handler's circuit breaker
```

### 5.3 Scaling

- **Need:** from Little's law, busy workers needed = arrival rate x mean job time; from M/M/c, the smallest worker count that keeps predicted p99 wait under the target.
- **Can it help:** if the pool is at or above the core count and workers are CPU-bound (CPU time close to busy time), more workers cannot help; do not add any (I-23, I-37).
- **Act:** move toward the target by at most the rate cap per interval; never past floor or cap; never reverse within the cooldown; hysteresis between the up and down thresholds.
- **Retire:** one worker at a time, after it finishes its job; never below the floor.
- The control law (model-based, AIMD, or PID) is chosen by W-12 *(pending I-21)*.

### 5.4 Failing safe

- The controller parks with no timer when every lane is empty and no job runs; the next submit wakes it.
- If it panics, it is caught and restarted. Workers do not need it to run jobs.
- Workers check its heartbeat on their slow path (when they are about to park); a stale heartbeat makes one worker restart it *(pending I-26)*.

## 6. Time: delays and cron

- A hierarchical timing wheel per pool with occupancy bitmaps, so the next due time is found without scanning; cancellation marks the entry and the wheel skips it.
- The timer thread sleeps until the next due time with a high-resolution timer (on Windows, a high-resolution waitable timer, not a condvar timeout) *(pending I-33)*.
- Cron: parsed once; the next fire time is computed field by field. Evaluated in UTC by default. A time zone is an explicit option; DST rule: a skipped local time runs once at the next valid time, a repeated one runs once.

## 7. Failures, retries, leases

- Backoff: `min(cap, base * 2^attempt)` with full jitter (a uniform draw in `[0, that]`).
- Retry budget per handler: a token bucket refilled by successful attempts; a retry spends a token; with none left, the job waits in its lane's retry set instead of running *(pending I-22)*.
- Circuit breaker per handler: closed, open after the failure threshold in the window, half-open after the cool-off with one probe job.
- Leases: each attempt gets a 64-bit fencing token from a per-store counter. `extend_lease` renews it. The controller expires leases of stuck jobs.

## 8. The store and the durable log

### 8.1 The `Store` contract

Transitions reach a store in batches through `apply` (so a durable store flushes once per batch: group commit), and leasing is its own call; both return only after every transition is applied (memory) or on stable storage (durable). The trait is drafted in `API.md`, Tier 3. The transitions:

| Call | Effect |
|---|---|
| `put(job)` | adds a pending job; refuses a duplicate key inside the window |
| `lease(lane, n, token_base)` | marks up to `n` pending jobs as leased with tokens; returns them |
| `complete(id, token)` | completed, if the token is current |
| `retry(id, token, at, reason)` | back to pending at `at`, attempt + 1, reason appended |
| `dead_letter(id, token, reason)` | moved to the DLQ with every reason |
| `expire(id, token)` | lease expired: back to pending (controller repair) |
| `recover()` | after a restart: every non-terminal job, leased ones returned to pending |

Every implementation, task-io's own and every adapter's, must also hold to these, and the contract tests check each one *(I-39, I-40)*:

- **A transition is one conditional step.** `lease` moves a job from pending to leased only if it is still pending, in the same operation that reads it, so two concurrent `lease` calls never return the same job. `complete`, `retry`, `dead_letter`, and `expire` apply only if the token is current.
- **A keyed `put` is one step.** The duplicate check and the insert happen together; there is no window between a lookup and an insert in which a second `put` with the same key gets in.
- **A stale token is an outcome, not an error.** A transition refused because the lease moved returns a typed `Stale` result. A worker records it as an event and moves on. No store result, error or otherwise, ends a worker; a worker that ends outside shutdown is dead to the controller and is replaced.
- **A transition touches only its own job** *(I-41)*. Every other record, and the store's counters other than the ones the transition names, are unchanged.
- **Tokens only grow, across restarts too.** The token counter is part of the store's durable state: every token issued after `recover()` is greater than every token issued before the crash, so a result from a worker that outlived the crash is always stale.
- **An attempt is counted when the job is leased**, not when its handler returns. A job whose handler crashes the process, or hangs until its lease expires, still uses up attempts and reaches the DLQ instead of being retried forever.
- **Times in the store are wall-clock milliseconds since the Unix epoch** (`u64`), so they survive a restart. Lease expiry inside a running pool is judged on the monotonic clock; after a restart every lease is expired by definition (`recover()` returns leased jobs to pending).
- **Limits are checked before anything is committed**: a `put` past the store's job limit returns `Full`, and a record past its size limit is refused before it is allocated.

### 8.2 The durable log format *(pending I-11, I-18)*

- A directory of segment files, `00000001.log`, `00000002.log`, ..., each up to 64 MiB.
- Each record:

  ```
  | u32 length | u32 CRC-32C of (type .. payload) | u8 type | u64 job id | u64 token | ... fields | payload |
  ```

  Types: `Put`, `Lease`, `Complete`, `Retry`, `DeadLetter`, `Expire`, `Checkpoint`.
- **Group commit:** submits append to an in-memory buffer; the flusher writes the buffer and calls `File::sync_data`, which is `fdatasync` on Linux, `fcntl(F_FULLFSYNC)` on macOS (where `fsync` alone does not reach stable storage), and `FlushFileBuffers` on Windows (read in the std source for 1.95; to re-check against 1.85 at the lock), then acknowledges every submit in that batch. The batch window adapts: a lone submit is flushed at once; under load, batches grow.
- **Recovery:** read segments from the last checkpoint; a record whose length runs past the end of the file, or whose CRC does not match, ends recovery for that segment (a torn tail from a crash) and is truncated; a bad record in the middle of a segment is reported, its job goes to the DLQ, and recovery continues with the next valid record.
- **Compaction:** a checkpoint record lists live jobs; segments wholly before the last checkpoint are deleted.
- **Encryption:** with a `Sealer`, the payload and fields are sealed before the CRC is computed over the sealed bytes, so corruption is caught before decryption.

### 8.3 The conformance suite *(draft for the design lock)*

`task_io::store::conformance::run(make_store)` runs every check below against a store built by `make_store` (a fresh one per check). task-io's memory and durable stores pass it in CI on every platform, and an adapter crate passes it in its own CI before it can be listed. The suite ships in the library and uses no dependency: its random cases come from a seeded SplitMix64 generator, and a failing case prints its seed so it can be replayed.

| Check | What it proves | How |
|---|---|---|
| C-1 one-step lease | two leases never return the same job | 32 threads lease from one lane of 100,000 jobs until it is empty; every job leased exactly once |
| C-2 one-step keyed put | a key inside its window is stored once | 32 threads put the same 10,000 keys; exactly one `Applied` per key, every other result `Duplicate` naming the stored job |
| C-3 stale tokens | a moved lease cannot be completed | lease, expire, lease again; `complete`, `retry`, and `dead_letter` with the first token return `Stale`, and the record is unchanged |
| C-4 isolation (I-41) | a transition touches only its job | 10^6 random transitions on random jobs; after each, every other record compares equal to a reference copy |
| C-5 state machine | only legal transitions apply | random sequences against a reference model of `Pending -> Leased -> (Completed / RetryWait -> Pending / DeadLettered)`; illegal ones return a typed outcome and change nothing |
| C-6 attempts | attempts are counted at lease | a job leased and expired `max_attempts` times without a result is dead-lettered with every reason |
| C-7 time | a job is never leased before it is due | `retry(at)` and delayed `put`; leasing at `at - 1 ms` returns nothing, at `at` returns the job |
| C-8 limits | bounded, refused before allocation | fill to the job limit, then `Full`; an oversized record is refused, and the allocation count does not grow for it (the suite ships `conformance::CountingAlloc`; a test binary that installs it as its global allocator gets the allocation check, one that does not gets the refusal check only) |
| C-9 tokens across restart (durable stores) | fencing holds through a crash | issue tokens, crash, `recover()`; every new token is greater than every old one, and a pre-crash token completes nothing |
| C-10 no lost acknowledged job (durable stores) | durability | a child process puts and transitions jobs and records each acknowledgment; it is killed at random points (10,000 seeds); after `recover()`, every acknowledged transition is present, and nothing unacknowledged is shown as completed |
| C-11 corruption (durable stores) | hostile bytes never become jobs | random bit flips, truncations, and appended garbage in the store's files; `recover()` reports each damaged record, never panics, never returns a damaged job as valid, and keeps every undamaged one |
| C-12 dedupe window across restart (durable stores) | a key stays refused inside its window after a crash | put with a key, crash, recover, put the key again inside the window: `Duplicate`; after the window: `Applied` |
| C-13 group commit (durable stores) | a batch costs one flush | `apply` with 1,000 transitions issues one flush (counted through the store's flush hook) and acknowledges all of them |

## 9. Shutdown

1. Stop admission (`SubmitError::ShuttingDown`).
2. Stop the timer and the controller.
3. Let running handlers finish until the deadline.
4. Return every unstarted and leased-but-not-started job to pending in the store.
5. Join workers; report handlers still running at the deadline as abandoned (their results will be fenced off).
6. Flush and close the durable log.

## 10. Isolated workers

- `register_isolated` starts a child pool lazily. Each child is the same binary started with an argument that `worker_main()` recognizes.
- Parent and child talk over two pipes with length-prefixed frames (size-checked, fuzzed): `Run(job)`, `Done(id, result)`, `Heartbeat`.
- The controller treats children like workers: a hung child is killed (`TerminateProcess` / `kill`) and replaced; a crashed child's job is retried under a new lease.
- Children inherit no handles but their pipes *(pending I-25)*.

## 11. The `unsafe` map

`unsafe` lives only in `queue/` and `sys/` (DIRECTIVES section 2). Everything `std` already does safely is taken from `std`: flushing (`File::sync_data`), killing a child (`Child::kill`), the CPU count under cgroup quotas and affinity (`available_parallelism`), and raw thread and process handles (`AsRawHandle`, `JoinHandleExt::as_pthread_t`). This table is the complete planned list; anything added later is added here first, with its test.

### 11.1 `queue/`

| Id | Operation | Invariant | How the code establishes it | Proof |
|---|---|---|---|---|
| Q-1 | Read and write a ring cell (`UnsafeCell<MaybeUninit<Slot>>`) | only the thread that owns a position touches its cell: a producer after winning the tail CAS while `seq == pos`, a consumer after winning the head CAS while `seq == pos + 1`; each hands the cell over with a `Release` store of `seq` that the next owner reads with `Acquire` | Vyukov's bounded MPMC protocol, as in `compare/src/proto.rs` | loom: 2 producers and 2 consumers on rings of 2 and of 4 cells, every interleaving; Miri on the unit tests |
| Q-2 | Store a closure inline (`ptr::write` into a 64-byte, 16-aligned buffer) and run or drop it through a function pointer | the `call` and `drop` pointers are instantiated for the same `F` that was written; exactly one of them runs, once; a closure that does not fit (size or alignment) is boxed and the buffer holds its pointer | `SmallJob::new` picks the path and both pointers together; `run` and `drop` consume `self` | Miri with drop-counting, panicking, zero-sized, and over-aligned closures (the boxed path); a job dropped at shutdown runs its `drop`, never its `call` |
| Q-3 | `unsafe impl Send for SmallJob`, `unsafe impl Sync` for the ring | a `SmallJob` is built only from `F: Send`; ring cells are shared only under Q-1 | the constructor's bound; Q-1 | `compile_fail` doctests: a non-`Send` closure does not submit (no dev-dependency needed) |
| Q-4 | Out-of-line payloads (`Box::into_raw`, `Box::from_raw`) | the raw pointer is owned by exactly one slot at a time and freed exactly once, by `run` or `drop` | part of Q-2's single consumption | Miri leak check over the shutdown and drop paths |

The idle mask, the searching count, the health records, and the timer wheel are plain atomics and indexes: no `unsafe`. Parking goes through `std::thread::park` unless I-04 or I-34 measures a gain from parking on a word directly; then the call is in `sys/` (S-W4, S-L4, S-M4) and the protocol keeps its loom model.

### 11.2 `sys/`

Each OS call is wrapped in a safe function that checks the return code (a failure becomes `io::Error::last_os_error()`, never a panic), sizes every buffer before the call, and never reads past the length the OS reports or past the buffer, whichever is smaller. Every `// SAFETY:` comment names the documentation listed in section 12. Each wrapper has a test on its OS in CI. A function missing on an older OS version (S-W5, S-M4) is resolved at run time and falls back; it is never assumed.

## 12. The `sys/` surface

Planned FFI declarations per OS, with the documentation each relies on. A pending item names the experiment that decides whether the call is needed at all.

### 12.1 Windows (`kernel32`, and the `api-ms-win-core-synch-l1-2-0` API set)

| Id | Need | Function | Reference |
|---|---|---|---|
| S-W1 | topology: cores, SMT siblings, caches, processor groups | `GetLogicalProcessorInformationEx(RelationAll, buf, &mut len)`: a first call with a null buffer returns `ERROR_INSUFFICIENT_BUFFER` and the length; records are variable-size, and each record's `Size` is bounds-checked before it is read | Microsoft Learn, "GetLogicalProcessorInformationEx", "SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX" |
| S-W2 | pin or place a worker *(pending I-06)* | `SetThreadGroupAffinity(GetCurrentThread(), &GROUP_AFFINITY, null)` (hard); `SetThreadIdealProcessorEx` (hint) | Microsoft Learn, "SetThreadGroupAffinity", "SetThreadIdealProcessorEx" |
| S-W3 | per-thread CPU time for the controller | `GetThreadTimes` on the worker's `JoinHandle` raw handle: kernel and user time in 100 ns units, updated at the scheduler tick, so coarse over short intervals; `QueryThreadCycleTime` is exact but in cycles. Which one the controller uses is decided with its sampling interval *(pending I-21)* | Microsoft Learn, "GetThreadTimes", "QueryThreadCycleTime" |
| S-W4 | park on a word *(pending I-04, I-34)* | `WaitOnAddress`, `WakeByAddressSingle`, `WakeByAddressAll` (Windows 8 and later, `Synchronization.lib`) | Microsoft Learn, "WaitOnAddress" |
| S-W5 | sub-millisecond timer for due jobs *(pending I-33)* | `CreateWaitableTimerExW` with `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`, `SetWaitableTimer` (a negative due time is relative, in 100 ns units), `WaitForMultipleObjects` with a wake event for an earlier deadline, `CloseHandle`. The flag exists from Windows 10 1803; if creation fails, a normal waitable timer is used | Microsoft Learn, "CreateWaitableTimerExW" |
| S-W6 | isolated children die with the parent (no orphans) | `CreateJobObjectW`, `SetInformationJobObject(JobObjectExtendedLimitInformation)` with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, `AssignProcessToJobObject` on the `Child`'s raw handle | Microsoft Learn, "JOBOBJECT_EXTENDED_LIMIT_INFORMATION" |

Write-through (`FILE_FLAG_WRITE_THROUGH`) for the durable log, if I-18 wants it, is `OpenOptionsExt::custom_flags`: safe, no FFI.

### 12.2 Linux (`libc`, which `std` already links)

| Id | Need | Function | Reference |
|---|---|---|---|
| S-L1 | topology | `/sys/devices/system/cpu/cpu*/topology/` and `cache/index*/shared_cpu_list`, read with `std::fs` (safe; size-limited reads parsed as untrusted text), restricted to the CPUs `sched_getaffinity(0, len, set)` allows; the set is grown and the call retried on `EINVAL`, for hosts past 1,024 CPUs | `sched_getaffinity(2)`; kernel `Documentation/ABI/stable/sysfs-devices-system-cpu` |
| S-L2 | pin or place a worker *(pending I-06)* | `sched_setaffinity(0, len, set)` from the worker itself (pid 0 is the calling thread) | `sched_setaffinity(2)` |
| S-L3 | per-thread CPU time | `pthread_getcpuclockid(pthread_t, &mut clockid)`, then `clock_gettime(clockid, &mut ts)`, with the `pthread_t` from the worker's `JoinHandle`; valid because the pool holds the handle until it joins the thread | `pthread_getcpuclockid(3)`, `clock_gettime(2)` |
| S-L4 | park on a word *(pending I-04, I-34)* | `syscall(SYS_futex, addr, FUTEX_WAIT_PRIVATE, expected, null, null, 0)` and `FUTEX_WAKE_PRIVATE`; `SYS_futex` is 202 on x86_64 and 98 on aarch64 and riscv64. `std::thread::park` is already futex-based on Linux, so this is added only for a measured gain | `futex(2)` |
| S-L5 | isolated children die with the parent | the child calls `prctl(PR_SET_PDEATHSIG, SIGKILL)` at start, then compares `getppid()` with the parent id it was given and exits if they differ (the parent may have died before the call) | `prctl(2)`, `getppid(2)` |

### 12.3 macOS (`libSystem`)

| Id | Need | Function | Reference |
|---|---|---|---|
| S-M1 | topology, including performance and efficiency cores | `sysctlbyname` for `hw.physicalcpu`, `hw.logicalcpu`, `hw.nperflevels`, `hw.perflevel{N}.physicalcpu`, `hw.perflevel{N}.l2cachesize`, `hw.cachelinesize` (128 on Apple silicon); every value is read into a buffer of the expected size, and a missing key (older or Intel Macs) is skipped | `sysctl(3)`; Apple, "Determining system capabilities" |
| S-M2 | placement *(pending I-06)* | macOS has no hard affinity; `pthread_set_qos_class_self_np(QOS_CLASS_USER_INITIATED, 0)` steers workers to performance cores, `QOS_CLASS_UTILITY` to efficiency cores for low-priority lanes | `pthread/qos.h` |
| S-M3 | per-thread CPU time | `thread_info(pthread_mach_thread_np(pthread_t), THREAD_BASIC_INFO, &mut info, &mut count)`: user and system time with microsecond resolution; `count` is checked on return | `mach/thread_info.h` |
| S-M4 | park on a word *(pending I-04, I-34)* | `os_sync_wait_on_address`, `os_sync_wake_by_address_any` (macOS 14.4 and later), looked up with `dlsym(RTLD_DEFAULT, ..)` at start; when missing, `std::thread::park` | `os/os_sync_wait_on_address.h` |

Flushing needs no FFI on any OS (section 8.2).
