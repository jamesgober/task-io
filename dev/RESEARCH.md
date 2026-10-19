# task-io - R&D Program

> How task-io finds designs that beat every existing job pipeline and worker pool, and the register of every idea tried. Read after [`PLAN.md`](./PLAN.md) and [`BENCHMARKS.md`](./BENCHMARKS.md). The main R&D round is `0.2.0`; the second round is `0.7.0`; ideas can be added at any time.

---

## 1. Why this exists

Copying an existing job queue gets its numbers. The mandate (PLAN 4.1) is at least 1.20 times the best competitor on every benchmark, so the design has to come from measurement, not habit. This file is where ideas are written down, tested, and either adopted with evidence or dropped with evidence.

---

## 2. Rules

1. **Every idea is an entry in section 4** before any code is written for it, with: id, hypothesis, method (the experiment), expected cost (latency, memory, complexity, security effect), success criterion, status, and evidence.
2. **Experiments are code, not opinions.** Each experiment is a small, self-contained program under `.dev/experiments/<id>/` (private) or a suite in `compare/` (public) that anyone can rerun. The result, the command that produced it, and the host are written in the entry.
3. **Status values:** `proposed`, `running`, `adopted`, `dropped`, `parked`. `adopted` needs a measured win on the success criterion and a decision in PLAN section 14. `dropped` needs the measurement that killed it. `parked` needs the reason and the condition that would revive it.
4. **Security and stability are part of the criterion.** An idea that wins on speed but breaks an invariant in DIRECTIVES section 4 is dropped, or fixed and measured again.
5. **Read before you build.** For each area, read the sources in section 5 and write a short note (what the source does, what its limit is, what we would do differently) into `.dev/research/<area>.md`.
6. **Invention quota.** The `0.2.0` design must adopt at least three capabilities that no benchmarked competitor offers (PLAN D-014). Each one is proven by an experiment, not argued.
7. **New ideas are required, not optional.** The `0.2.0` round adds at least ten new entries to section 4 beyond the ones listed here, from reading, profiling, and failed experiments.
8. **Unsoundness is never an option.** An idea that requires undefined behavior, or a soundness argument nobody can write down, is dropped.
9. **Stay in scope.** Ideas for a general executor, actors, or spawns go to `_strategy/EXECUTOR-SEED/` or `_strategy/SPAWN-IO.md`, not here (PLAN D-015).

---

## 3. Method for one experiment

1. Write the entry (section 4) with the hypothesis and success criterion **before** measuring.
2. Build the smallest prototype that tests the hypothesis and nothing else.
3. Measure it against the current design and the best competitor using the BENCHMARKS protocol, on R1 and R2.
4. Profile it (Windows: WPA / ETW; Linux: `perf stat`, `perf record`, cache-miss and branch-miss counters).
5. Write the verdict, the numbers, and the command into the entry. Adopt, drop, or park.

---

## 4. Idea register

| Id | Idea | Hypothesis | Method | Expected cost | Success criterion | Status |
|---|---|---|---|---|---|---|
| I-01 | **Job slots in fixed rings** instead of boxed jobs in a channel | a pre-sized ring of job slots removes the allocator and pointer chasing from submit and dispatch | prototype ring vs crossbeam-pool on W-01, W-02, W-04 | memory reserved up front per lane | W-01 at least 1.20; W-04 0 allocations | running (section 6) |
| I-02 | **Batch dequeue**: a worker takes up to K jobs per queue operation | one contended operation per K jobs instead of per job | W-01 with K = 1, 4, 16, 64 at 32 workers | latency of the last job in a batch; fairness | W-01 gain with W-02 p99 not worse | proposed |
| I-03 | **Per-worker local queues with stealing** vs **one shared lane queue** | under 32 submitters, local queues and stealing beat one MPMC queue; with few submitters the shared queue wins; pick by measurement or adapt | both prototypes on W-01 (1 and 32 submitters), W-06 | lane ordering is harder with local queues | the faster design on every row, or an adaptive switch with no loss | proposed |
| I-04 | **Eventcount parking** (Vyukov) instead of a mutex and condvar | no lost wakeups, no herd, lower wake latency | loom model; W-02, W-05 | subtle; loom required | W-02 p99 at least 1.20; W-05 0 wakeups | running (section 6) |
| I-05 | **Adaptive spin before park**, tuned from measured arrival gaps | spinning briefly beats parking when jobs arrive microseconds apart; idle CPU stays zero | W-02, W-05 | wasted CPU if mistuned | W-02 gain with W-05 still at 0 wakeups | running (section 6) |
| I-06 | **Topology-aware placement**: workers and their lanes grouped by core complex (CCD) | keeping a lane's workers on one CCD cuts cross-CCD cache traffic on the 9950X3D | measure cross-CCD vs same-CCD cache-line transfer cost, then W-01, W-03 with and without | topology code in `sys/` per OS | measurable win on R1, no loss on uniform hosts | proposed |
| I-07 | **Hierarchical timer wheel** for delayed jobs, lazy cancellation, tickless | beats a binary heap on insert and cancel, keeps lateness low, no idle ticks | D-01 vs heap-scheduler and tokio | wheel memory | D-01 at least 1.20; never early | proposed |
| I-08 | **Deficit round robin across lanes and tenants** at zero cost for a single lane | weighted fairness without a branch on the single-lane fast path | W-06, W-07, and W-01 with one lane | per-lane state | share error at most 2%; W-01 single-lane within 2% of no-lanes | proposed |
| I-09 | **End-to-end back-pressure** with a deadline on `submit` | overload produces waiting at the source and typed refusals, not memory growth | W-08 | API must stay simple | W-08 absolute targets met; p99 at least 1.20 | proposed |
| I-10 | **Work rescue**: a watchdog moves a stuck handler's lane to a spare worker and fences the late result | other jobs keep their latency even with a handler that blocks | W-10, P-03 | a watchdog thread, woken only while jobs run | W-10 p99 at least 1.20; P-03 0 stale commits | proposed |
| I-11 | **Group commit** in task-io's own durable log store (PLAN D-018) | batching flushes gives far more durable throughput at the same durability, with the batch window adapting to load | P-01 vs apalis on SQLite and the other durable competitors | latency of the batch window | P-01 throughput and p99 at least 1.20 | proposed |
| I-12 | **Leases with fencing tokens** (Gray and Cheriton 1989; Kleppmann 2016) in-process, not only across nodes | a late result from a stalled handler can never commit | P-03, fault-injected simulation | one token per attempt | 0 stale commits across 10,000 runs | proposed |
| I-13 | **Time-bucketed dedupe index** with an optional probabilistic prefilter | bounded memory and O(1) expiry of the whole oldest bucket; the prefilter answers "never seen" without touching the index | K-01 | false positives of the prefilter must fall through to the exact index, never refuse wrongly | K-01 0 duplicates; submit cost with a key at least 1.20 better than competitors that dedupe | proposed |
| I-14 | **Deterministic simulation mode** (seeded clock, interleaving, store faults) | seeded replay finds bugs in task-io and in users' handlers that stress tests miss | build `sim`; 10,000 seeds over the pipeline; count bugs found vs the stress suite | a second scheduler to keep in sync | finds at least one class of bug the stress suite misses; zero cost when off | proposed |
| I-15 | **Per-worker observability** with no shared atomics; histograms merged on read | full events and metrics cost under 1% on W-01 | W-01 with and without a sink | memory per worker | less than 1% overhead with a sink | proposed |
| I-16 | **Inline small payloads** and zero-copy handoff of large ones | most job payloads are small (ids, keys); storing them in the slot removes an allocation and a cache miss | W-01, W-04 with payload sizes 8, 64, 512, 4096 bytes | slot size | 0 allocations up to the inline size; W-01 gain | proposed |
| I-17 | **Cron next-fire computed directly** from the parsed expression | no per-tick scan of schedules; idle stays at 0 wakeups with 10,000 schedules | D-02 | parser complexity; fuzz the parser | D-02 at least 1.20; W-05 still 0 wakeups with schedules registered | proposed |
| I-18 | **Append-only job log with checksums** as the store's record format (for durable stores) | sequential writes and fast recovery scans beat update-in-place | P-01, P-02 recovery time | compaction | P-01 and P-02 at least 1.20 | proposed |
| I-19 | **Aging** inside lanes | a job that waited too long is lifted so no job starves, without hurting the share accuracy | W-06 with a long-running low lane | a timestamp check per pick | max wait bounded; share error still at most 2% | proposed |
| I-20 | **Executor seam at zero cost** when unused | the trait adds no branch or size to the sync handler path | W-01 with and without the `exec` feature | API surface | W-01 within 1% | proposed |
| I-21 | **The controller's control law** (PLAN 5.4, D-019): compare additive increase with multiplicative decrease, a PID controller on queue wait, and a queueing-model estimate (Little's law and M/M/c sizing from measured arrival and service rates); from James's elastic idea (`_strategy/SPAWN-IO.md` 4.7) | a model-based controller reaches the right size faster and with fewer reversals than a pure feedback loop | W-12 for each law, in simulation first, then on R1 and R2 | complexity; tuning | W-12 p99 at least 1.20 vs tokio-blocking and best-fixed; at most 2 reversals per step | proposed |
| I-22 | **Retry budgets and a per-handler circuit breaker** | a downstream outage costs a bounded number of attempts and recovers faster | R-02 | tuning | R-02 waste at least 1.20 lower; drain time at least 1.20 faster | proposed |
| I-23 | **CPU-time-aware scaling**: compare each worker's busy time with its per-thread CPU time (`sys/`) to tell blocked workers from CPU-bound ones | adding workers only when they are blocked avoids the oversubscription that fixed and naive elastic pools suffer; no competitor does this | W-12 both handler kinds, with and without the signal | one OS call per worker per sample, off the hot path; per-OS code | W-12 CPU rows: no worker past the core count and p99 at least 1.20; IO rows: settles faster than tokio-blocking | proposed |
| I-24 | **Slow-worker detection by median absolute deviation** of job time among workers running the same handler | catches a degraded worker (noisy neighbor, throttled core, poisoned thread-local state) without false alarms from slow jobs | W-13 slow-worker case; false-positive count over S-01 | per-handler per-worker statistics in the controller | detected within 3 sampling windows; 0 false positives in 24 hours | proposed |
| I-25 | **Isolated workers** in child processes with task-io's own framing over pipes (PLAN 5.5, D-020) | process isolation makes stuck or crashing handlers killable at a cost low enough for jobs over about 1 ms | isolated vs threaded on W-01, W-02 with 1 ms handlers; kill-and-replace time | per-job IPC; child start time; per-OS process code | overhead under 10% for 1 ms handlers; kill-and-replace p99 under 50 ms (estimate, to be measured) | proposed |
| I-26 | **A watchdog for the watchdog**: workers check the controller's heartbeat on their slow path and restart it if stale | the controller can never become a single point of failure | inject controller panics and hangs in simulation | one load on a slow path | the pool keeps running and the controller is back within 2 sampling windows | proposed |
| I-27 | **Health records as single-writer, cache-line padded, relaxed stores** read by the controller with no locks | full per-worker health costs under 1% of throughput | W-14 against a lock-based and a shared-atomic design; loom model of the read protocol | memory per worker | W-14 at most 1% | proposed |
| I-28 | **CRC-32C with hardware acceleration** (SSE 4.2 on x86-64, the CRC extension on AArch64) in `sys/`, with a portable fallback | checksumming every record costs almost nothing next to the flush | P-01 with and without checksums | unsafe intrinsics behind `cfg`; Miri on the fallback | checksum cost under 2% of P-01 latency | proposed |
| I-29 | **Admission by expected wait, not by slot count** (CoDel-style): refuse or hold a submit when the lane's measured drain time already exceeds the job's deadline | a bounded queue alone does not bound latency: in the 2026-10-14 quick trial, crossbeam-bounded refused 23% of jobs at 2x load yet its admitted p99 was 1.6 s, because 65,536 slots hold 1.6 s of work at 4 workers | W-08 with a time-based admission rule vs slot-based | a per-lane drain-rate estimate | admitted p99 within 2x of unloaded (the W-08 absolute target) with refusals no higher than crossbeam-bounded | proposed |
| I-30 | **Default worker count below the logical CPU count**, leaving room for submitters and the controller | at 32 workers plus a submitter on 32 logical CPUs, crossbeam-pool and tokio-blocking swung between 0.3 s and 3.8 s per million jobs in the quick trial; oversubscription makes spin-then-park thrash | W-01 and W-02 at 16, 24, 28, 32 workers on R1 | none | stable W-01 (95% CI width under 10% of the median) at the default size, and no loss against the best size | proposed |
| I-31 | **Never replace a thread on a handler panic**: catch it and keep the worker | with 10% of jobs panicking, `threadpool` (which respawns the thread) ran 234k jobs/s against 3.3M/s for a pool that catches the panic; catching is nearly free on the happy path | W-09 | none | W-09 at least 1.20 vs the best contender; 0 workers lost | proposed |
| I-32 | **Shutdown hands unstarted jobs back to the store** instead of dropping them | in the quick trial, tokio tasks ran 233 of 200,000 queued jobs before `shutdown_timeout` returned and rayon ran none; losing queued work at shutdown is the common failure | W-11 with the memory and durable stores | a drain path in the store | 0 jobs lost; time to stop at least 1.20 better than the best contender that also loses nothing | proposed |
| I-33 | **High-resolution timers on Windows** for the delay wheel's wakeups (`CreateWaitableTimerExW` with `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`) instead of condvar timeouts | both the heap scheduler and tokio fired delayed jobs about 8 ms late at p50 on R1 in the quick trial, which matches the default Windows timer granularity, not the schedulers | D-01 on R1 with condvar timeouts vs a high-resolution waitable timer; same on R2 for comparison | per-OS code in `sys/`; a little more power use when timers are near | D-01 lateness p99 at least 1.20 better on both hosts, and under 1 ms on R1 | proposed |
| I-34 | **Wake by address** (`WaitOnAddress` / `futex`) for parked workers instead of a mutex and condvar | idle submit-to-start p50 was about 9.6 us for crossbeam-pool on R1; most of it is the park and unpark path | W-02 idle with each mechanism, inside the eventcount of I-04 | per-OS code in `sys/` | W-02 idle p50 and p99 at least 1.20 better | proposed |
| I-35 | **Per-submitter staging buffers** flushed in batches to the lane | with 4 workers, crossbeam-pool took 45 ms for a million jobs from one submitter but 122 ms from 32 submitters: the shared queue head is the bottleneck | W-01 32-submitter rows | memory per submitter; a flush on idle so nothing waits | 32-submitter W-01 at least 1.20 vs the best contender, and no worse than the 1-submitter row | running (section 6) |
| I-36 | **A job slot of at most 80 bytes** for jobs with inline payloads | queued jobs cost 104 B (crossbeam-pool), 112 B (rayon), and 256 B (tokio) in the quick trial; a fixed slot with a 64-byte inline payload and a 16-byte header beats all of them | W-04 | payloads over 64 bytes go out of line | W-04 at least 1.20 below the best contender | running (section 6) |
| I-37 | **Elastic growth only when it helps** (the evidence for I-23) | tokio's elastic blocking pool grew to 515 threads for 100 us CPU-only jobs during the W-12 step, far past the 32 logical CPUs, where extra threads only add contention; the best fixed pool for that step was 32 workers | W-12 CPU rows: compare thread count and p99 against tokio-blocking-elastic | none beyond I-23 | peak threads at most the logical CPU count; p99 at least 1.20 better | proposed |
| I-39 | **One-step claim and one-step keyed insert** (the evidence for the store contract in ARCHITECTURE 8.1) | fang 0.11 claims a job with a read and then an unconditional write, and checks a unique key with a lookup and then an insert; under 32 workers both race, so jobs run twice and repeated keys are stored twice. A transition that is one conditional step (state and token checked in the same operation) cannot race | store contract tests: 32 threads leasing the same lane, 32 threads putting the same keys, on the memory store, the durable store, and every adapter; loom model of the memory store's lease | none on the memory store (the transition is already one atomic write, PLAN 5.2); the durable store checks the key and the state inside its single writer | 0 jobs leased twice, 0 duplicate keys stored, in every test and model | proposed |
| I-40 | **Workers never leave on an error** | fang 0.11's worker returns out of its loop on any store error after a job (for example, losing the claim race above), and its pool restarts only workers that panicked, so each such error shrinks the pool for good without a log line; over months a pool like that drains to zero. A task-io worker treats a store error as an event plus a backoff, never an exit, and the controller replaces any worker that ends outside shutdown (D-019) | a fault-injecting store that fails a set share of calls, run for 10^7 jobs: worker count, lost jobs, events | none on the hot path; one event per store error | worker count stays at its target; 0 lost jobs; every store error has an event | proposed |
| I-38 | **Panic-free submission paths under concurrency** | apalis-core 1.0.0's in-memory backend panics when a push races its worker (`try_lock().unwrap()`); every submit path in task-io must be safe under any interleaving, which loom and the simulation mode prove | loom model of submit racing dispatch; 10,000 simulation seeds with concurrent submitters | none | 0 panics, 0 lost jobs in every model and seed | proposed |

---

## 5. Reading list

Read each, write the note (rule 5), and cite it in the entries it affects.

**Queues and worker pools**
- Dmitry Vyukov's bounded MPMC queue, intrusive MPSC queue, and eventcount.
- Chase and Lev, "Dynamic Circular Work-Stealing Deque", SPAA 2005; Le, Pop, Cohen, Zappa Nardelli, "Correct and Efficient Work-Stealing for Weak Memory Models", PPoPP 2013 (for I-03).
- The crossbeam-channel, rayon-core (sleep module, thread pool), and tokio blocking-pool sources.
- Linux `futex(2)` and `futex_waitv`; Windows `WaitOnAddress`; macOS `os_sync_wait_on_address`.

**Fair scheduling and overload**
- Shreedhar and Varghese, "Efficient Fair Queuing using Deficit Round Robin", SIGCOMM 1995.
- Welsh, Culler, Brewer, "SEDA: An Architecture for Well-Conditioned, Scalable Internet Services", SOSP 2001.
- Nichols and Jacobson, "Controlling Queue Delay" (CoDel), ACM Queue 2012, for queue-delay based admission.
- Google SRE book, chapters on handling overload and addressing cascading failures (retry budgets).

**Timers and cron**
- Varghese and Lauck, "Hashed and Hierarchical Timing Wheels", SOSP 1987; the tokio time driver source.
- The cron expression grammar (Vixie cron, and the Quartz extensions) for the parser and its edge cases (day-of-month vs day-of-week, DST changes).

**Durability, leases, idempotency**
- Gray and Cheriton, "Leases", SOSP 1989; Kleppmann, "How to do distributed locking", 2016.
- The Sidekiq reliability model, Faktory, and the transactional outbox pattern.
- Temporal and Cadence durable execution (for what not to make users pay for).
- Group commit in database logs (for I-11); store-io's own design notes.
- Bloom, cuckoo, and xor filters (for I-13).

**Control loops and autoscaling**
- Hellerstein, Diao, Parekh, Tilbury, "Feedback Control of Computing Systems", Wiley 2004.
- Little's law and M/M/c queueing (any standard text), for model-based sizing.
- The Go runtime's `sysmon` (it finds goroutines stuck in system calls and hands their processor to another thread): the closest existing design to the controller.
- The tokio blocking-pool source (thread growth, keep-alive, shutdown).
- Kubernetes Horizontal Pod Autoscaler and AWS target-tracking scaling (stabilization windows, cooldowns), as reference designs.

**Testing**
- FoundationDB simulation testing; TigerBeetle VOPR; madsim; AWS shuttle; loom; Miri.
- Kingsbury's Jepsen analyses (method, for 0.6.0).

**Hardware**
- AMD Zen 5 and 3D V-Cache topology documentation; measured cross-CCD latency on R1.

---

## 6. Findings

Filled in by each R&D round: the short version of what was learned, with links to the entries.

**2026-10-14, quick trials on R1 (harness check runs, not for gating; full-protocol results replace these):**
- The hand-rolled crossbeam pool is the strongest dispatch baseline so far (about 45 ns per empty job at 4 workers). Several contenders become unstable at 32 workers on 32 logical CPUs (I-30).
- Bounding a queue by slots does not bound latency (I-29).
- Replacing threads on panic is very slow; catching is nearly free (I-31).
- Most contenders lose or abandon queued work at shutdown (I-32).
- Delayed-job lateness on Windows is dominated by timer granularity, about 8 ms (I-33).
- tokio's elastic pool oversubscribes CPU-bound work by about 16x (I-37, I-23).
- apalis's in-memory backend cannot take concurrent submissions (I-38).

**2026-10-19, first prototype (`proto-ring` in `compare/src/proto.rs`), quick W-01 and W-04 on R1 (host shared with other builds, so indicative only):**
- I-01 + I-16: a Vyukov ring with inline job slots ran 1M empty jobs in 111 to 124 ms against 102 to 154 ms for crossbeam-pool at 4 workers and 1 submitter: no clear win yet at low worker counts.
- I-04, first design: waking one worker per published job made the pool 5x slower than crossbeam-pool at 32 workers (988 ms against 209 ms): one `unpark` per job. With at most one searching worker and I-05's short spin before parking, the same run took 109 ms against 201 ms for crossbeam-pool (1.84x), and 139 ms against 161 ms with 32 submitters.
- A lost-wakeup deadlock was found in the searching protocol: a waker could count a worker as searching in the window after it announced idleness, and if that worker's re-check found a job, the count leaked and no one was woken again. Fixed by giving the search back on that path. This is exactly the class of bug the loom models in DIRECTIVES section 2 exist to catch; task-io's version of this protocol is not written without one.
- I-35 is needed: with 32 submitters at 4 workers, the shared ring tail made the prototype no faster than crossbeam-pool.
- I-36: W-04's job (64-byte payload plus 16 bytes of harness state) did not fit the 64-byte inline area, so it was boxed: 340 B effective per queued job against 104 B for the best competitor. The 1.20 target (at most about 87 B effective, start reservation included) means the real slot must hold payload, header, and queue cell in about 86 bytes; a separate sequence word per cell (the prototype's 96-byte cell) does not fit.
- P-02 against apalis on SQLite: after a kill -9 and restart, a worker that reused the crashed worker's name processed nothing for at least 120 s (apalis registers workers by name, and the dead one's registration had not expired); with a unique name per process it resumed in 57 ms with 0 lost jobs and 0 duplicates. task-io's recovery must never wait on an old identity to expire: a restarted pool reclaims its own leases at once (fencing tokens make that safe), and the harness gives apalis the unique-name configuration.

**2026-10-19, fang 0.11 on SQLite, quick runs of P-02, R-01, R-02, and K-01 on R1 (host shared with other builds; the timings are indicative only, the counts are not load-dependent):**
- fang runs jobs twice. It claims a job with a `SELECT` and then a separate unconditional `UPDATE ... SET state = 'in_progress'` (no transaction; the PostgreSQL and MySQL queries add `FOR UPDATE SKIP LOCKED`, but the lock ends with the `SELECT` statement). With 32 workers: 13 to 24 duplicate runs in 2,000 R-01 jobs (three runs), 33 to 42 duplicate effects in 2,000 P-02 jobs (I-39).
- fang's unique jobs are not unique under concurrency: the check is a lookup and then an insert, and the hash index is not unique. K-01 with 32 submitters stored 53 to 62 duplicate keyed tasks out of 1,000 keys and ran 62 to 81 keys twice (I-39).
- fang's pool loses workers for good. A store error after a job (the loser of the claim race gets `RowNotFound` from `schedule_retry` or `found: 0` from `remove_task`; under load also `database is locked`) returns out of the worker's loop, and the pool's supervisor restarts a worker only when its task panicked. 25 of 32 workers left the pool during one 2,000-job R-01 run, 9 in R-02, 9 in K-01, with no log line from fang (I-40).
- A job that was `in_progress` when its worker left is never fetched again (only `new` and `retried` are), so two of three quick R-02 runs stalled with 1 to 2 jobs stranded. fang has no lease or visibility timeout.
- After kill -9, fang resumed in 54 to 110 ms with 0 lost jobs in these runs; P-02's lost-job count can be nonzero whenever the kill lands between a claim and the handler's effect, since nothing returns an `in_progress` job to the queue.
- These rows run fang on SQLite. BENCHMARKS asks for fang's fastest local backend, and PostgreSQL has not been measured yet (it is not installed on R1).

**2026-10-19, `proto-shards` (I-35), quick W-01, W-02, W-04, W-11 on R1 against `proto-ring` and crossbeam-pool (host shared with other builds; one quick pass, so indicative only):**
- Design: proto-ring's pool and wake protocol unchanged, on a different queue. The lane's 65,536 slots are split into 16 rings, each submitting thread gets its own ring on first use, and workers drain the rings in place, starting from the ring that last had work. Staged jobs are never held back by their submitter (no flush step), threads past 16 share a ring safely (the rings are MPMC), and a full ring spills into the next, so capacity and the W-04 reservation are unchanged.
- W-01, 1,000,000 empty jobs: 24.6 ms against 137 ms (proto-ring) and 149 ms (crossbeam-pool) at 32 workers and 32 submitters, 6.1x the crossbeam-pool median; 51.4 ms against 124 ms at 4 workers and 32 submitters, with a wide interval (42 to 116 ms). The 1-submitter rows also improved (64.5 ms against 109 ms at 4 and at 32 workers); the cause is not yet known (one guess: a full ring spills into the next, spreading one submitter's jobs over rings) and must be found before this is trusted.
- W-02 is mixed: at 4 workers and 80% busy, p99 125 us against 292 us (crossbeam-pool) and 20 ms (proto-ring, an outlier to re-check); idle p99 446 us against 85 us (proto-ring). The 32-worker, 80%-busy rows are all tens of milliseconds for every contender: the host could not sustain 80% on 32 workers while other builds ran. W-02 must be re-run on a quiet host before any verdict.
- W-04 and W-11: same as proto-ring (bounded at 65,536; every accepted job ran at shutdown, 0 lost).
- Ordering: no lane promises FIFO start order today (PLAN 5.6 orders lanes by priority, not jobs within a lane). Sharding keeps one submitter's jobs in start order while they stay on its ring and loses that when a ring spills. Whether a lane should promise per-submitter order is a design-lock question, decided by measurement of what it costs.
- Next: a quiet-host run of W-01 to W-03 with 1, 4, 16, and 32 submitters, the shard count swept (4, 8, 16, 32), and the 1-submitter speedup explained by profile.

**2026-10-19, the survey contenders (affinitypool, rusty_pool, executors, kanal-pool), quick W-01, W-04, W-11, W-12, W-13 on R1 (host shared with other builds; indicative only):**
- None ran without problems at every size. affinitypool was the fastest W-01 row once (64.8 ms against 133 ms for crossbeam-pool at 4 workers and 32 submitters) and 359 ms in a second pass of the same row; at 32 workers and 1 submitter it took 3.1 to 3.4 s, against 297 ms for crossbeam-pool. Replacing the adapter's handle bookkeeping with a leak made no difference (3.4 s), so the cost is affinitypool's own.
- affinitypool keeps 176 B per queued job in W-04 (crossbeam-pool: 104 B). A first pass showed 16 B: the adapter still held the warm-up batch's finished handles and freed them inside the measured batch. W-04 now calls a `settle` hook after the warm-up, a no-op for every other contender.
- executors' `shutdown` dropped every queued job in W-11 (200,000 accepted, 0 run), as its source shows; it cannot set that bar.
- kanal-pool was not faster than crossbeam-pool except at 32 workers and 32 submitters (116 ms against 151 ms); at 32 workers and 1 submitter it took 919 ms.
- rusty-pool-elastic, the new elastic row: W-13 (one handler hangs, 85% load) p99 0.13 ms with 0 jobs later than 100 ms, because it adds a thread when no worker is idle; fixed pools at 4 workers had p99 1.4 to 4.8 s. W-12, 1 ms blocking handlers: p99 0.19 ms with 63 peak threads. Controller experiments I-21 and I-23 must beat this row, not only tokio's elastic pool.
