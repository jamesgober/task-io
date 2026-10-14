# task-io - R&D Program

> How task-io finds designs that beat every existing runtime, and the register of every idea tried. Read after [`PLAN.md`](./PLAN.md) and [`BENCHMARKS.md`](./BENCHMARKS.md). The main R&D round is `0.2.0`; the second round is `0.7.0`; ideas can be added at any time.

---

## 1. Why this exists

Copying tokio's design gets tokio's numbers. The mandate (PLAN 4.1) is at least 1.20 times the best competitor on every benchmark, so the design has to come from measurement, not from habit. This file is where ideas are written down, tested, and either adopted with evidence or dropped with evidence.

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
| I-01 | **One pool for every kind of work**, with Go-style handoff when a worker runs blocking work | one thread set beats tokio's separate blocking pool on memory and on async latency under mixed load | prototype handoff; E-08, S-02, E-10 | complexity in the parking protocol; must not add a branch to the async poll path | E-08 p99 and E-10 memory at or above the 1.20 factor | proposed |
| I-02 | **Zero-allocation spawn**: per-worker size-class slabs, recycled task memory, batch refill across workers | removing the allocator from spawn cuts E-01 and E-03 time by more than 20% | counting allocator plus E-01, E-03, E-05, E-10 | memory held in slabs; must return memory to the OS after a burst (bounded slab growth) | 0 allocations per spawn in steady state; E-01 factor at least 1.20 | proposed |
| I-03 | **Lazy task creation** (Mohr, Kranz, Halstead): a spawned task joined by its parent before anyone steals it runs inline with no queue traffic | most fork-join children are never stolen; skipping the queue removes most of the cost | E-03, E-07 against rayon | needs a cheap "was it stolen" check | E-07 at least 1.20 vs rayon | proposed |
| I-04 | **Heartbeat scheduling** (Acar et al., PLDI 2018) for fork-join: create parallel tasks only on a periodic heartbeat, keep the rest sequential | bounded overhead even with no cutoff (fib(40) without cutoff) where eager spawning loses | E-07 no-cutoff variant | a timer or counter per worker | E-07 no-cutoff at least 1.20 vs rayon | proposed |
| I-05 | **Topology-aware stealing**: steal within the L3 group (CCD) first, across CCDs only after a threshold | cross-CCD traffic on the 9950X3D is the largest avoidable cost in steal-heavy loads | measure cross-CCD vs intra-CCD cache-line transfer cost, then E-03, A-06 with and without | topology code in `sys/` per OS | measurable win on R1 with no loss on uniform hosts | proposed |
| I-06 | **V-Cache-aware placement**: on hosts with asymmetric L3 (9950X3D), place cache-heavy work on the large-cache CCD | memory-bound tasks run faster on the V-Cache CCD | a memory-bound benchmark pinned to each CCD | detection of asymmetric caches; must be a no-op elsewhere | measured gain on cache-heavy workloads | proposed |
| I-07 | **Eventcount parking** (Vyukov) instead of mutex plus condvar | no lost wakeups, no herd, lower wake latency | loom model, E-02, E-04, E-09 | subtle; loom required | E-02 p99 at least 1.20 | proposed |
| I-08 | **Adaptive spin before park** tuned from measured wake latency | spinning briefly beats parking when work arrives within microseconds; idle CPU stays zero | E-02, E-04, E-09 | wasted CPU if mistuned | E-02 gain with E-09 still at 0 wakeups | proposed |
| I-09 | **Handoff scheduling for message chains**: when task A sends on a channel that wakes idle task B, B runs next on the same worker (LIFO slot), with a bound | message chains stay in cache; spawn-io and actor-io inherit the win | E-04, C-01 | fairness bound needed | E-04 and C-01 at least 1.20 | proposed |
| I-10 | **Sound async scopes** that let children borrow from the parent, despite the scoped-task trilemma | a design exists that is sound and costs nothing on the non-scoped path (for example a scope that blocks on drop, or a scope that owns the borrowed data) | design notes plus Miri; read "The Scoped Task trilemma" (Mandry, 2023) | API complexity | a sound design with a Miri-clean test suite, or a written proof that none fits, and the blocking scope only | proposed |
| I-11 | **Work rescue**: a watchdog detects a task stuck in one poll and moves the rest of that worker's queue to a spare | tail latency stays bounded even with a misbehaving task (Rust cannot preempt a future) | S-02 | watchdog thread wakes, but only when work is running | S-02 p99 at least 1.20 vs tokio | proposed |
| I-12 | **Deterministic simulation mode** built into the runtime | seeded replay finds races in task-io and in user code that stress tests miss | build `sim`, run 10,000 seeds over the scheduler, monitors, and jobs, count bugs found vs the stress suite | a second scheduler implementation to keep in sync | finds at least one class of bug the stress suite misses; zero cost when the feature is off | proposed |
| I-13 | **Monitors in the task header**: the exit cause is delivered by the worker that finished the task, with no extra thread and no allocation | supervisors built on task-io (spawn-io, actor-io) learn of a failure faster than any runtime that makes them poll or join | E-13 | one pointer-sized field per task, or a side table | E-13 at least 1.20; no change to E-01 or E-10 beyond 2% | proposed |
| I-14 | **Deadline lanes** (earliest deadline first per worker) alongside the normal queue | latency-sensitive tasks keep p99 under load while batch work fills the rest | S-01 with mixed classes | queue complexity | latency-class p99 at least 1.20 vs tokio under S-01 | proposed |
| I-15 | **Weighted fair lanes per tenant** (deficit round robin) | one tenant cannot starve another; costs nothing when only one tenant exists | multi-tenant variant of S-01 | per-lane state | isolation holds; single-tenant numbers unchanged within 2% | proposed |
| I-16 | **End-to-end back-pressure**: channel and queue capacity propagate to `spawn` and `send` | overload produces waiting at the source, not memory growth | S-01 | API must stay simple | S-01 absolute targets met | proposed |
| I-17 | **Group commit for durable jobs** through store-io's Submitter (batched fsync) | durable enqueue throughput beats competitors by a wide margin at equal durability | J-02 | latency of the batch window | J-02 throughput and p99 at least 1.20 | proposed |
| I-18 | **Leases with fencing tokens** for jobs and distributed work (Gray and Cheriton 1989; Kleppmann 2016) | stale workers can never commit a result after their lease moved | Jepsen-style test, crash suite | a token per lease | 0 stale commits across 10,000 fault-injected runs | proposed |
| I-19 | **Virtual actors** (Orleans) | moved to actor-io (PLAN D-015) | | | | parked: revive in actor-io |
| I-20 | **Thread-per-core profile** of the same runtime (glommio and monoio style) selected by configuration | one codebase can match thread-per-core runtimes on their own benchmarks | E-01 to E-04 against glommio and monoio on R2 | a second scheduling mode | at least 1.20 vs glommio and monoio on R2 | proposed |
| I-21 | **Per-worker observability** with no shared atomics; histograms merged on read | full metrics cost under 1% on E-01 | E-01 with and without a sink | memory per worker | less than 1% overhead with a sink attached | proposed |
| I-22 | **Cooperative budget with automatic yield** at await points of the crate's own primitives | no task starves others without user changes | E-06 | one counter per poll | E-06 max gap at least 1.20 better than tokio | proposed |
| I-23 | **Stackful option for LexerSketch** (green threads with small stacks) on the same scheduler | scripts that call blocking host functions keep the async workers free | prototype with a context-switch routine in `sys/`; E-03 vs `may` | unsafe surface, per-OS context switch | E-03 at least 1.20 vs `may`; Miri and loom clean where they apply | proposed |
| I-24 | **The spawn model (James)** | described by James 2026-10-13 in `_strategy/SPAWN-IO.md`; built in spawn-io (PLAN D-015). task-io's part is the list in PLAN 5.10 | | | | moved to spawn-io |

---

## 5. Reading list

Read each, write the note (rule 5), and cite it in the entries it affects.

**Scheduling and work stealing**
- Blumofe and Leiserson, "Scheduling Multithreaded Computations by Work Stealing", JACM 1999.
- Chase and Lev, "Dynamic Circular Work-Stealing Deque", SPAA 2005.
- Le, Pop, Cohen, Zappa Nardelli, "Correct and Efficient Work-Stealing for Weak Memory Models", PPoPP 2013.
- Mohr, Kranz, Halstead, "Lazy Task Creation", 1990.
- Acar, Chargueraud, Guatto, Rainey, Sieczkowski, "Heartbeat Scheduling", PLDI 2018.
- Acar, Chargueraud, Rainey, "Scheduling Parallel Programs by Work Stealing with Private Deques", PPoPP 2013.
- Carl Lerche, "Making the Tokio scheduler 10x faster", 2019, and the current tokio scheduler source.
- Dmitry Vyukov, "Scalable Go Scheduler Design", 2012; the Go non-cooperative preemption proposal (Austin Clements), 2019.
- The rayon-core source (join, scope, sleep module).

**Microsecond-scale systems and tail latency**
- Ousterhout et al., "Shenango", NSDI 2019; Kaffes et al., "Shinjuku", NSDI 2019; Fried et al., "Caladan", OSDI 2020.
- Enberg, Rao, Tarkoma, "The Impact of Thread-Per-Core Architecture on Application Tail Latency", 2019.
- Seastar, glommio, and monoio design documents.

**Lock-free structures and parking**
- Vyukov's eventcount, intrusive MPSC queue, and bounded MPMC queue.
- Michael, "Hazard Pointers", 2004; crossbeam-epoch.
- Dice, Marathe, Shavit, "Lock Cohorting", 2012 (NUMA).
- Linux `futex(2)` and `futex_waitv`; Windows `WaitOnAddress`; macOS `os_sync_wait_on_address`.

**Timers**
- Varghese and Lauck, "Hashed and Hierarchical Timing Wheels", SOSP 1987; the tokio time driver source.

**Supervision (for monitors here, and for spawn-io and actor-io later)**
- Armstrong, "Making reliable distributed systems in the presence of software errors", 2003 (OTP supervision).
- Bernstein et al., "Orleans: Distributed Virtual Actors for Programmability and Scalability", MSR 2014.
- The Pony runtime scheduler and ORCA.
- Imam and Sarkar, "Savina - An Actor Benchmark Suite", AGERE 2014.

**Jobs and distribution**
- Gray and Cheriton, "Leases", SOSP 1989; Kleppmann, "How to do distributed locking", 2016.
- Temporal and Cadence durable execution model; the transactional outbox pattern.
- Kingsbury's Jepsen analyses (method).

**Testing**
- FoundationDB simulation testing; TigerBeetle VOPR; madsim; AWS shuttle; loom; Miri (Stacked Borrows and Tree Borrows papers).
- Mandry, "The Scoped Task trilemma", 2023.

**Hardware**
- AMD Zen 5 and 3D V-Cache topology documentation; measured cross-CCD latency on R1.

---

## 6. Findings

Filled in by each R&D round: the short version of what was learned, with links to the entries. Empty until `0.2.0`.
