//! Experiment I-07 (dev/RESEARCH.md): a hierarchical timing wheel for
//! delayed jobs (Varghese and Lauck 1987; the layout tokio's time driver
//! uses: 6 levels of 64 slots, 1 tick per slot at level 0), with lazy
//! cancellation and an exact next deadline.
//!
//! The change from tokio: when the earliest occupied slot is on a higher
//! level, tokio wakes at that slot's start to cascade it; this wheel reports
//! the earliest live deadline inside that slot instead, so the timer thread
//! never wakes without a job to fire. That is exact because an entry is
//! placed on the lowest level whose span holds its distance from now, so
//! every entry on a higher level, or later in the same level, is due later.
//!
//! Timers further out than the wheel's span (64^6 ticks) wait in a binary
//! heap and move into the wheel when they come within it, so the order of
//! far timers is kept exactly too; tokio instead files them in the top
//! level by a clamped deadline, which loses their order.
//!
//! Written to task-io's rules (no dependency, no `unsafe`, no panic on any
//! input, bounded memory: entries live in a slab with a fixed capacity).

const LEVELS: usize = 6;
const SLOT_BITS: u32 = 6;
const SLOTS: usize = 1 << SLOT_BITS;
/// Deadlines past this many ticks from now go to the top level's last span
/// and are cascaded again when they come near (64^6 ticks: 2.2 years at
/// 1 ms).
const MAX_SPAN: u64 = 1 << (SLOT_BITS * LEVELS as u32);

use std::cmp::Reverse;
use std::collections::BinaryHeap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TimerId {
    index: u32,
    generation: u32,
}

struct Entry {
    deadline: u64,
    generation: u32,
    live: bool,
    payload: u64,
}

pub struct Wheel {
    /// Ticks processed so far: every deadline up to here has fired.
    elapsed: u64,
    slots: Vec<Vec<u32>>, // LEVELS * SLOTS lists of slab indexes
    occupied: [u64; LEVELS],
    entries: Vec<Entry>,
    free: Vec<u32>,
    capacity: usize,
    live: usize,
    /// Timers past `limit()` when they were filed, earliest first. Every
    /// one is due after every timer in the slots (moved in by `migrate`).
    overflow: BinaryHeap<Reverse<(u64, u32)>>,
    /// Cancelled entries not yet dropped from their slot or the heap. They count
    /// against `capacity` too, so insert-and-cancel churn cannot grow memory.
    pending: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum WheelError {
    /// The wheel holds `capacity` timers already (bounded memory).
    Full,
}

impl Wheel {
    pub fn new(capacity: usize) -> Self {
        Self {
            elapsed: 0,
            slots: (0..LEVELS * SLOTS).map(|_| Vec::new()).collect(),
            occupied: [0; LEVELS],
            entries: Vec::with_capacity(capacity.min(u32::MAX as usize)),
            free: Vec::new(),
            overflow: BinaryHeap::new(),
            capacity: capacity.min(u32::MAX as usize),
            live: 0,
            pending: 0,
        }
    }

    /// Live timers (the tests compare it with the model).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.live
    }

    /// The last tick the slots can hold: the tick before the top level's
    /// current slot comes round again. A deadline past it would land in that
    /// current slot, which `advance` takes as due, files again in the same
    /// slot, and loops on without end (found by `matches_reference`).
    fn limit(&self) -> u64 {
        let top = SLOT_BITS * (LEVELS as u32 - 1);
        (self.elapsed & !((1u64 << top) - 1)).saturating_add(MAX_SPAN - 1)
    }

    /// Level and slot for `deadline` (at most `limit()`) relative to
    /// `self.elapsed`.
    fn place(&self, deadline: u64) -> (usize, usize) {
        let deadline = deadline.min(self.limit());
        let masked = (self.elapsed ^ deadline) | (SLOTS as u64 - 1);
        let significant = 63 - masked.leading_zeros();
        let level = (significant / SLOT_BITS) as usize;
        let level = level.min(LEVELS - 1);
        let slot = ((deadline >> (level as u32 * SLOT_BITS)) as usize) & (SLOTS - 1);
        (level, slot)
    }

    fn link(&mut self, index: u32) {
        let deadline = self.entries[index as usize].deadline;
        if deadline > self.limit() {
            self.overflow.push(Reverse((deadline, index)));
            return;
        }
        let (level, slot) = self.place(deadline);
        self.slots[level * SLOTS + slot].push(index);
        self.occupied[level] |= 1 << slot;
    }

    /// Adds a timer firing at tick `deadline` (a past deadline fires on the
    /// next `advance`; nothing ever fires early).
    pub fn insert(&mut self, deadline: u64, payload: u64) -> Result<TimerId, WheelError> {
        if self.live + self.pending >= self.capacity {
            if self.pending > 0 {
                self.sweep();
            }
            if self.live + self.pending >= self.capacity {
                return Err(WheelError::Full);
            }
        }
        let deadline = deadline.max(self.elapsed);
        let index = match self.free.pop() {
            Some(i) => {
                let e = &mut self.entries[i as usize];
                e.generation = e.generation.wrapping_add(1);
                e.deadline = deadline;
                e.live = true;
                e.payload = payload;
                i
            }
            None => {
                self.entries.push(Entry {
                    deadline,
                    generation: 0,
                    live: true,
                    payload,
                });
                (self.entries.len() - 1) as u32
            }
        };
        self.live += 1;
        self.link(index);
        Ok(TimerId {
            index,
            generation: self.entries[index as usize].generation,
        })
    }

    /// Cancels a timer: O(1), marked and skipped later (lazy). False if it
    /// already fired or was cancelled.
    pub fn cancel(&mut self, id: TimerId) -> bool {
        match self.entries.get_mut(id.index as usize) {
            Some(e) if e.live && e.generation == id.generation => {
                e.live = false;
                self.live -= 1;
                self.pending += 1;
                true
            }
            _ => false,
        }
    }

    /// Frees a fired entry's slab slot.
    fn release(&mut self, index: u32) {
        self.free.push(index);
    }

    /// Frees a cancelled entry's slab slot.
    fn release_cancelled(&mut self, index: u32) {
        self.pending -= 1;
        self.free.push(index);
    }

    /// Moves heap timers that are now within `limit()` into the slots, so
    /// the slots again hold every timer due by then.
    fn migrate(&mut self) {
        let limit = self.limit();
        while let Some(&Reverse((deadline, index))) = self.overflow.peek() {
            if deadline > limit {
                break;
            }
            self.overflow.pop();
            if self.entries[index as usize].live {
                self.link(index);
            } else {
                self.release_cancelled(index);
            }
        }
    }

    /// The earliest live deadline in the heap; drops cancelled ones on top.
    fn overflow_min(&mut self) -> Option<u64> {
        while let Some(&Reverse((deadline, index))) = self.overflow.peek() {
            if self.entries[index as usize].live {
                return Some(deadline);
            }
            self.overflow.pop();
            self.release_cancelled(index);
        }
        None
    }

    /// Drops every cancelled entry from every slot and the heap (when full).
    fn sweep(&mut self) {
        let heap = std::mem::take(&mut self.overflow);
        let mut kept = Vec::with_capacity(heap.len());
        for Reverse((deadline, index)) in heap.into_vec() {
            if self.entries[index as usize].live {
                kept.push(Reverse((deadline, index)));
            } else {
                self.release_cancelled(index);
            }
        }
        self.overflow = BinaryHeap::from(kept);
        for k in 0..LEVELS * SLOTS {
            if self.slots[k].is_empty() {
                continue;
            }
            let list = std::mem::take(&mut self.slots[k]);
            let mut kept = Vec::with_capacity(list.len());
            for i in list {
                if self.entries[i as usize].live {
                    kept.push(i);
                } else {
                    self.release_cancelled(i);
                }
            }
            if kept.is_empty() {
                self.occupied[k / SLOTS] &= !(1 << (k % SLOTS));
            }
            self.slots[k] = kept;
        }
    }

    /// The earliest occupied (level, slot, slot start) at or after `elapsed`.
    fn next_slot(&self) -> Option<(usize, usize, u64)> {
        for level in 0..LEVELS {
            let occ = self.occupied[level];
            if occ == 0 {
                continue;
            }
            let shift = level as u32 * SLOT_BITS;
            let now_slot = ((self.elapsed >> shift) as usize) & (SLOTS - 1);
            // Slots at or after the current one, wrapping around.
            let rotated = occ.rotate_right(now_slot as u32);
            let ahead = rotated.trailing_zeros() as usize;
            let slot = (now_slot + ahead) & (SLOTS - 1);
            let span = 1u64 << shift;
            let level_start = self.elapsed & !((span << SLOT_BITS) - 1);
            let mut start = level_start + slot as u64 * span;
            if start + span <= self.elapsed {
                // The slot index wrapped: it belongs to the next rotation.
                start += span << SLOT_BITS;
            }
            return Some((level, slot, start));
        }
        None
    }

    /// The earliest deadline of a live timer, exactly; drops cancelled
    /// entries it meets on the way.
    pub fn next_deadline(&mut self) -> Option<u64> {
        loop {
            let Some((level, slot, _)) = self.next_slot() else {
                // Slots empty: the heap's earliest is the earliest of all.
                return self.overflow_min();
            };
            let list = std::mem::take(&mut self.slots[level * SLOTS + slot]);
            let mut kept = Vec::with_capacity(list.len());
            let mut min = u64::MAX;
            for i in list {
                let e = &self.entries[i as usize];
                if e.live {
                    min = min.min(e.deadline);
                    kept.push(i);
                } else {
                    self.release_cancelled(i);
                }
            }
            if kept.is_empty() {
                self.occupied[level] &= !(1 << slot);
                continue;
            }
            self.slots[level * SLOTS + slot] = kept;
            return Some(min);
        }
    }

    /// Moves time to `now` and calls `fire` for every live timer due by
    /// then, in deadline order across slots (same-tick order unspecified).
    /// Returns how many fired.
    pub fn advance(&mut self, now: u64, mut fire: impl FnMut(TimerId, u64, u64)) -> usize {
        let mut fired = 0;
        loop {
            self.migrate();
            let Some((level, slot, start)) = self.next_slot().filter(|s| s.2 <= now) else {
                // No slot due by `now`. If a heap timer is, every timer in
                // the slots is later still, so time can jump to it.
                match self.overflow_min() {
                    Some(d) if d <= now => {
                        self.elapsed = self.elapsed.max(d);
                        continue;
                    }
                    _ => break,
                }
            };
            let list = std::mem::take(&mut self.slots[level * SLOTS + slot]);
            self.occupied[level] &= !(1 << slot);
            // The wheel is now at the slot's start; entries in it are due
            // within its span.
            self.elapsed = self.elapsed.max(start);
            let mut due: Vec<(u64, u32)> = Vec::new();
            for i in list {
                let e = &self.entries[i as usize];
                if !e.live {
                    self.release_cancelled(i);
                } else if e.deadline <= now {
                    due.push((e.deadline, i));
                } else {
                    // Not due yet: place it again, closer to now.
                    self.link(i);
                }
            }
            due.sort_unstable();
            for (deadline, i) in due {
                let e = &mut self.entries[i as usize];
                e.live = false;
                self.live -= 1;
                let id = TimerId {
                    index: i,
                    generation: e.generation,
                };
                fire(id, deadline, e.payload);
                self.release(i);
                fired += 1;
            }
        }
        self.elapsed = self.elapsed.max(now);
        self.migrate();
        fired
    }
}

/// One single-threaded run of the I-07 micro-measurement: `n` timers with
/// deadlines spread over `window` ticks, every other one cancelled, then time
/// advanced in steps of `step` ticks until all have fired. No lock, no
/// thread: the data structure alone. Returns ns per insert, per cancel, and
/// per fired timer (the drain, including the steps).
pub struct Micro {
    pub insert: f64,
    pub cancel: f64,
    pub drain: f64,
}

fn deadlines(n: usize, window: u64) -> Vec<u64> {
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % window
        })
        .collect()
}

pub fn micro_wheel(n: usize, window: u64, step: u64) -> Micro {
    use std::time::Instant;
    let ds = deadlines(n, window);
    let mut w = Wheel::new(n);
    let t = Instant::now();
    let ids: Vec<TimerId> = ds
        .iter()
        .enumerate()
        .map(|(i, &d)| w.insert(d, i as u64).expect("capacity is n"))
        .collect();
    let insert = t.elapsed();
    let t = Instant::now();
    for id in ids.iter().step_by(2) {
        w.cancel(*id);
    }
    let cancel = t.elapsed();
    let t = Instant::now();
    let mut sum = 0u64;
    let mut fired = 0;
    let mut now = 0;
    while now < window + step {
        now += step;
        fired += w.advance(now, |_, _, p| sum = sum.wrapping_add(p));
    }
    let drain = t.elapsed();
    std::hint::black_box(sum);
    assert_eq!(fired, n / 2, "wheel lost timers");
    Micro {
        insert: insert.as_nanos() as f64 / n as f64,
        cancel: cancel.as_nanos() as f64 / n.div_ceil(2) as f64,
        drain: drain.as_nanos() as f64 / fired.max(1) as f64,
    }
}

/// The same run over a `BinaryHeap` with a cancelled flag per timer (the
/// heap-scheduler contender's structure).
pub fn micro_heap(n: usize, window: u64, step: u64) -> Micro {
    use std::time::Instant;
    let ds = deadlines(n, window);
    let mut cancelled = vec![false; n];
    let mut h: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::with_capacity(n);
    let t = Instant::now();
    for (i, &d) in ds.iter().enumerate() {
        h.push(Reverse((d, i as u32)));
    }
    let insert = t.elapsed();
    let t = Instant::now();
    for c in cancelled.iter_mut().step_by(2) {
        *c = true;
    }
    let cancel = t.elapsed();
    let t = Instant::now();
    let mut sum = 0u64;
    let mut fired = 0;
    let mut now = 0;
    while now < window + step {
        now += step;
        while let Some(&Reverse((d, i))) = h.peek() {
            if d > now {
                break;
            }
            h.pop();
            if !cancelled[i as usize] {
                sum = sum.wrapping_add(i as u64);
                fired += 1;
            }
        }
    }
    let drain = t.elapsed();
    std::hint::black_box(sum);
    assert_eq!(fired, n / 2, "heap lost timers");
    Micro {
        insert: insert.as_nanos() as f64 / n as f64,
        cancel: cancel.as_nanos() as f64 / n.div_ceil(2) as f64,
        drain: drain.as_nanos() as f64 / fired.max(1) as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashMap};

    /// A splitmix64 stream; `.1` biases delays to the far end of the span.
    struct Rng(u64, bool);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
        /// Delays spread over every level: 0, a few ticks, up to years.
        fn delay(&mut self) -> u64 {
            if self.1 && self.below(2) == 0 {
                // Around and past the span, where far timers are filed.
                return MAX_SPAN - (1 << 31) + self.below(MAX_SPAN * 3);
            }
            match self.below(5) {
                0 => 0,
                1 => self.below(64),
                2 => self.below(64 * 64 * 4),
                3 => self.below(1 << 24),
                _ => self.below(MAX_SPAN * 2),
            }
        }
        fn step(&mut self) -> u64 {
            if self.1 && self.below(3) == 0 {
                return self.below(MAX_SPAN);
            }
            match self.below(4) {
                0 => 1,
                1 => self.below(100),
                2 => self.below(1 << 16),
                _ => self.below(1 << 30),
            }
        }
    }

    #[test]
    fn fires_at_deadline_never_early() {
        let mut w = Wheel::new(16);
        let a = w.insert(5, 1).unwrap();
        let b = w.insert(4_000, 2).unwrap();
        w.insert(0, 3).unwrap();
        assert_eq!(w.next_deadline(), Some(0));
        let mut got = Vec::new();
        w.advance(4, |id, d, p| got.push((id, d, p)));
        assert_eq!(got.iter().map(|g| g.2).collect::<Vec<_>>(), vec![3]);
        assert_eq!(w.next_deadline(), Some(5));
        got.clear();
        w.advance(5, |id, d, p| got.push((id, d, p)));
        assert_eq!(got, vec![(a, 5, 1)]);
        assert_eq!(w.next_deadline(), Some(4_000));
        assert!(w.cancel(b));
        assert!(!w.cancel(b));
        assert_eq!(w.next_deadline(), None);
        assert_eq!(w.advance(1 << 40, |_, _, _| {}), 0);
    }

    #[test]
    fn cancel_churn_stays_bounded() {
        let mut w = Wheel::new(8);
        for k in 0..10_000u64 {
            let id = w.insert(1 << 30, k).unwrap();
            assert!(w.cancel(id));
        }
        assert!(w.entries.len() <= 8, "slab grew to {}", w.entries.len());
        assert_eq!(w.len(), 0);
    }

    #[test]
    fn bounded() {
        let mut w = Wheel::new(2);
        w.insert(10, 0).unwrap();
        w.insert(20, 0).unwrap();
        assert_eq!(w.insert(30, 0), Err(WheelError::Full));
        w.advance(10, |_, _, _| {});
        assert!(w.insert(30, 0).is_ok());
    }

    /// The wheel against an ordered map, over random inserts, cancels, and
    /// advances of every size: the same timers fire, none early, in deadline
    /// order, and `next_deadline` is always the earliest live deadline.
    #[test]
    fn matches_reference() {
        for seed in 0..200u64 {
            check(seed, false, 100_000);
        }
    }

    /// As `matches_reference`, with most timers beyond the wheel's span and
    /// time moving in jumps of up to a span (both bugs found so far were
    /// here).
    #[test]
    fn far_timers_match_reference() {
        for seed in 0..200u64 {
            check(seed, true, 100_000);
        }
    }

    /// As `matches_reference`, with the wheel kept full: inserts are
    /// refused only when `capacity` timers are live, after the sweep.
    #[test]
    fn full_wheel_matches_reference() {
        for seed in 0..200u64 {
            check(seed, seed % 2 == 0, 24);
        }
    }

    fn check(seed: u64, far: bool, capacity: usize) {
        {
            let mut r = Rng(seed, far);
            let mut w = Wheel::new(capacity);
            let mut model: BTreeMap<(u64, TimerId), u64> = BTreeMap::new();
            let mut by_id: HashMap<TimerId, u64> = HashMap::new();
            let mut now = r.below(1 << 40);
            w.advance(now, |_, _, _| {});
            for _ in 0..3_000 {
                match r.below(10) {
                    0..=4 => {
                        let deadline = now.saturating_add(r.delay());
                        let p = r.next();
                        let id = match w.insert(deadline, p) {
                            Ok(id) => id,
                            Err(WheelError::Full) => {
                                assert_eq!(model.len(), capacity, "seed {seed}: refused early");
                                continue;
                            }
                        };
                        model.insert((deadline, id), p);
                        by_id.insert(id, deadline);
                    }
                    5 | 6 => {
                        if let Some((&id, &d)) = by_id
                            .iter()
                            .nth(r.below(by_id.len().max(1) as u64) as usize)
                        {
                            assert!(w.cancel(id));
                            model.remove(&(d, id));
                            by_id.remove(&id);
                        }
                    }
                    _ => {
                        now = now.saturating_add(r.step());
                        let mut fired = Vec::new();
                        w.advance(now, |id, d, p| fired.push((d, id, p)));
                        let mut want = Vec::new();
                        while let Some((&(d, id), &p)) = model.iter().next() {
                            if d > now {
                                break;
                            }
                            model.remove(&(d, id));
                            by_id.remove(&id);
                            want.push((d, id, p));
                        }
                        assert!(
                            fired.windows(2).all(|x| x[0].0 <= x[1].0),
                            "seed {seed}: out of order"
                        );
                        fired.sort_unstable();
                        assert_eq!(fired, want, "seed {seed} at {now}");
                    }
                }
                assert_eq!(w.len(), model.len());
                assert_eq!(
                    w.next_deadline(),
                    model.keys().next().map(|k| k.0),
                    "seed {seed} at {now}"
                );
            }
        }
    }
}
