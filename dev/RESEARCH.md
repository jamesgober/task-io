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
| I-01 | **Job slots in fixed rings** instead of boxed jobs in a channel | a pre-sized ring of job slots removes the allocator and pointer chasing from submit and dispatch | prototype ring vs crossbeam-pool on W-01, W-02, W-04 | memory reserved up front per lane | W-01 at least 1.20; W-04 0 allocations | proposed |
| I-02 | **Batch dequeue**: a worker takes up to K jobs per queue operation | one contended operation per K jobs instead of per job | W-01 with K = 1, 4, 16, 64 at 32 workers | latency of the last job in a batch; fairness | W-01 gain with W-02 p99 not worse | proposed |
| I-03 | **Per-worker local queues with stealing** vs **one shared lane queue** | under 32 submitters, local queues and stealing beat one MPMC queue; with few submitters the shared queue wins; pick by measurement or adapt | both prototypes on W-01 (1 and 32 submitters), W-06 | lane ordering is harder with local queues | the faster design on every row, or an adaptive switch with no loss | proposed |
| I-04 | **Eventcount parking** (Vyukov) instead of a mutex and condvar | no lost wakeups, no herd, lower wake latency | loom model; W-02, W-05 | subtle; loom required | W-02 p99 at least 1.20; W-05 0 wakeups | proposed |
| I-05 | **Adaptive spin before park**, tuned from measured arrival gaps | spinning briefly beats parking when jobs arrive microseconds apart; idle CPU stays zero | W-02, W-05 | wasted CPU if mistuned | W-02 gain with W-05 still at 0 wakeups | proposed |
| I-06 | **Topology-aware placement**: workers and their lanes grouped by core complex (CCD) | keeping a lane's workers on one CCD cuts cross-CCD cache traffic on the 9950X3D | measure cross-CCD vs same-CCD cache-line transfer cost, then W-01, W-03 with and without | topology code in `sys/` per OS | measurable win on R1, no loss on uniform hosts | proposed |
| I-07 | **Hierarchical timer wheel** for delayed jobs, lazy cancellation, tickless | beats a binary heap on insert and cancel, keeps lateness low, no idle ticks | D-01 vs heap-scheduler and tokio | wheel memory | D-01 at least 1.20; never early | proposed |
| I-08 | **Deficit round robin across lanes and tenants** at zero cost for a single lane | weighted fairness without a branch on the single-lane fast path | W-06, W-07, and W-01 with one lane | per-lane state | share error at most 2%; W-01 single-lane within 2% of no-lanes | proposed |
| I-09 | **End-to-end back-pressure** with a deadline on `submit` | overload produces waiting at the source and typed refusals, not memory growth | W-08 | API must stay simple | W-08 absolute targets met; p99 at least 1.20 | proposed |
| I-10 | **Work rescue**: a watchdog moves a stuck handler's lane to a spare worker and fences the late result | other jobs keep their latency even with a handler that blocks | W-10, P-03 | a watchdog thread, woken only while jobs run | W-10 p99 at least 1.20; P-03 0 stale commits | proposed |
| I-11 | **Group commit** for durable submits through store-io's Submitter | batching flushes gives far more durable throughput at the same durability | P-01 vs apalis on SQLite | latency of the batch window | P-01 throughput and p99 at least 1.20 | proposed |
| I-12 | **Leases with fencing tokens** (Gray and Cheriton 1989; Kleppmann 2016) in-process, not only across nodes | a late result from a stalled handler can never commit | P-03, fault-injected simulation | one token per attempt | 0 stale commits across 10,000 runs | proposed |
| I-13 | **Time-bucketed dedupe index** with an optional probabilistic prefilter | bounded memory and O(1) expiry of the whole oldest bucket; the prefilter answers "never seen" without touching the index | K-01 | false positives of the prefilter must fall through to the exact index, never refuse wrongly | K-01 0 duplicates; submit cost with a key at least 1.20 better than competitors that dedupe | proposed |
| I-14 | **Deterministic simulation mode** (seeded clock, interleaving, store faults) | seeded replay finds bugs in task-io and in users' handlers that stress tests miss | build `sim`; 10,000 seeds over the pipeline; count bugs found vs the stress suite | a second scheduler to keep in sync | finds at least one class of bug the stress suite misses; zero cost when off | proposed |
| I-15 | **Per-worker observability** with no shared atomics; histograms merged on read | full events and metrics cost under 1% on W-01 | W-01 with and without a sink | memory per worker | less than 1% overhead with a sink | proposed |
| I-16 | **Inline small payloads** and zero-copy handoff of large ones | most job payloads are small (ids, keys); storing them in the slot removes an allocation and a cache miss | W-01, W-04 with payload sizes 8, 64, 512, 4096 bytes | slot size | 0 allocations up to the inline size; W-01 gain | proposed |
| I-17 | **Cron next-fire computed directly** from the parsed expression | no per-tick scan of schedules; idle stays at 0 wakeups with 10,000 schedules | D-02 | parser complexity; fuzz the parser | D-02 at least 1.20; W-05 still 0 wakeups with schedules registered | proposed |
| I-18 | **Append-only job log with checksums** as the store's record format (for durable stores) | sequential writes and fast recovery scans beat update-in-place | P-01, P-02 recovery time | compaction | P-01 and P-02 at least 1.20 | proposed |
| I-19 | **Aging** inside lanes | a job that waited too long is lifted so no job starves, without hurting the share accuracy | W-06 with a long-running low lane | a timestamp check per pick | max wait bounded; share error still at most 2% | proposed |
| I-20 | **Executor seam at zero cost** when unused | the trait adds no branch or size to the sync handler path | W-01 with and without the `exec` feature | API surface | W-01 within 1% | proposed |
| I-21 | **Elastic worker count** (additive increase, multiplicative decrease on measured throughput), from James's elastic-replication idea (`_strategy/SPAWN-IO.md` 4.7) | a pool that sizes itself beats a fixed pool on mixed workloads, especially when handlers block on IO | W-03 with mixed CPU and blocking handlers; W-10 | oscillation; thread churn | at least 1.20 on the mixed workload, no loss on pure CPU rows | proposed |
| I-22 | **Retry budgets and a per-handler circuit breaker** | a downstream outage costs a bounded number of attempts and recovers faster | R-02 | tuning | R-02 waste at least 1.20 lower; drain time at least 1.20 faster | proposed |

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

**Testing**
- FoundationDB simulation testing; TigerBeetle VOPR; madsim; AWS shuttle; loom; Miri.
- Kingsbury's Jepsen analyses (method, for 0.6.0).

**Hardware**
- AMD Zen 5 and 3D V-Cache topology documentation; measured cross-CCD latency on R1.

---

## 6. Findings

Filled in by each R&D round: the short version of what was learned, with links to the entries. Empty until `0.2.0`.
