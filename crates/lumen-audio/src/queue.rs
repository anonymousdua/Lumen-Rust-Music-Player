//! Play-queue ordering: shuffle, repeat and previous/next rules.
//!
//! Pure logic with no I/O, so it is fully unit tested. The engine keeps the
//! queue cursor on the *audible* track and asks [`Queue::peek_next_auto`] what to
//! pre-decode for gapless playback.
//!
//! Rules (from the spec):
//! - Shuffle plays every item once before any repeats (a permutation per cycle).
//! - Repeat off / all / one. Repeat one only affects automatic advance; manual
//!   next/previous still move (and wrap like repeat all).
//! - Previous restarts the track when more than 3 seconds have played.

use std::time::Duration;

use lumen_core::RepeatMode;

/// Previous restarts the current track once this much has played.
pub const PREV_RESTART_THRESHOLD: Duration = Duration::from_secs(3);

/// What [`Queue::prev`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrevAction {
    /// Seek the current item back to 0.
    Restart(usize),
    /// Play this item index from the start.
    Play(usize),
    /// The queue is empty.
    Nothing,
}

/// Small, fast, seedable RNG (SplitMix64). Good enough for shuffling.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Seeded from the clock.
    pub fn from_time() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        Self(nanos)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (n > 0).
    pub fn below(&mut self, n: usize) -> usize {
        // Lemire's multiply-shift; bias is negligible for queue sizes.
        ((self.next_u64() as u128 * n as u128) >> 64) as usize
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }
}

/// Queue ordering state over item indices `0..len`.
#[derive(Clone, Debug)]
pub struct Queue {
    len: usize,
    /// Play order (item indices). Identity when not shuffled.
    order: Vec<usize>,
    /// Position in `order` of the current item.
    cursor: Option<usize>,
    shuffle: bool,
    repeat: RepeatMode,
    /// Shuffled order for the next cycle, generated on demand so that
    /// `peek_next_auto` and `advance_auto` agree.
    next_cycle: Option<Vec<usize>>,
    rng: Rng,
}

impl Queue {
    pub fn new(rng: Rng) -> Self {
        Self {
            len: 0,
            order: Vec::new(),
            cursor: None,
            shuffle: false,
            repeat: RepeatMode::Off,
            next_cycle: None,
            rng,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    pub fn repeat(&self) -> RepeatMode {
        self.repeat
    }

    /// Item index of the current track.
    pub fn current(&self) -> Option<usize> {
        self.cursor.map(|c| self.order[c])
    }

    /// The play order (item indices).
    pub fn order(&self) -> &[usize] {
        &self.order
    }

    /// Replaces the queue with `len` items and makes `start` current.
    pub fn set_items(&mut self, len: usize, start: usize) {
        self.len = len;
        self.next_cycle = None;
        if len == 0 {
            self.order.clear();
            self.cursor = None;
            return;
        }
        let start = start.min(len - 1);
        if self.shuffle {
            self.order = self.shuffled_starting_with(start);
            self.cursor = Some(0);
        } else {
            self.order = (0..len).collect();
            self.cursor = Some(start);
        }
    }

    /// Makes `item` current (user picked a track).
    pub fn jump_to(&mut self, item: usize) -> Option<usize> {
        if item >= self.len {
            return None;
        }
        self.next_cycle = None;
        if self.shuffle {
            // A fresh cycle that starts with the chosen item.
            self.order = self.shuffled_starting_with(item);
            self.cursor = Some(0);
        } else {
            // The order is not always 0..len: "play next" inserts items.
            self.cursor = self.order.iter().position(|&i| i == item);
        }
        Some(item)
    }

    /// Adds `count` new items (indices `len..len + count`): right after the
    /// current item (`next`, "Play next") or at the end of the play order
    /// ("Add to queue"). The current item does not change.
    pub fn insert(&mut self, count: usize, next: bool) {
        if count == 0 {
            return;
        }
        let new = self.len..self.len + count;
        self.len += count;
        self.next_cycle = None;
        match self.cursor {
            Some(c) if next => {
                self.order.splice(c + 1..c + 1, new);
            }
            _ => self.order.extend(new),
        }
        if self.cursor.is_none() {
            self.cursor = Some(0);
        }
    }

    pub fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffle {
            return;
        }
        self.shuffle = on;
        self.next_cycle = None;
        if self.len == 0 {
            return;
        }
        let current = self.current().unwrap_or(0);
        if on {
            self.order = self.shuffled_starting_with(current);
            self.cursor = Some(0);
        } else {
            self.order = (0..self.len).collect();
            self.cursor = Some(current);
        }
    }

    pub fn set_repeat(&mut self, repeat: RepeatMode) {
        self.repeat = repeat;
    }

    /// What plays next when the current track ends by itself, without changing
    /// state. Used to pre-decode for gapless playback.
    pub fn peek_next_auto(&mut self) -> Option<usize> {
        let cursor = self.cursor?;
        if self.repeat == RepeatMode::One {
            return Some(self.order[cursor]);
        }
        if cursor + 1 < self.order.len() {
            return Some(self.order[cursor + 1]);
        }
        if self.repeat == RepeatMode::All {
            return Some(self.next_cycle_order()[0]);
        }
        None
    }

    /// The current track ended by itself. Returns the new current item, or
    /// `None` at the end of the queue (the cursor then stays on the last item).
    pub fn advance_auto(&mut self) -> Option<usize> {
        let cursor = self.cursor?;
        if self.repeat == RepeatMode::One {
            return Some(self.order[cursor]);
        }
        self.step_forward(self.repeat == RepeatMode::All)
    }

    /// The user pressed next. Wraps when repeat is all or one.
    pub fn next_manual(&mut self) -> Option<usize> {
        self.cursor?;
        self.step_forward(self.repeat != RepeatMode::Off)
    }

    /// The user pressed previous after `elapsed` of the current track.
    pub fn prev(&mut self, elapsed: Duration) -> PrevAction {
        let Some(cursor) = self.cursor else {
            return PrevAction::Nothing;
        };
        let current = self.order[cursor];
        if elapsed > PREV_RESTART_THRESHOLD {
            return PrevAction::Restart(current);
        }
        if cursor > 0 {
            self.cursor = Some(cursor - 1);
            self.next_cycle = None;
            return PrevAction::Play(self.order[cursor - 1]);
        }
        if self.repeat != RepeatMode::Off && self.order.len() > 1 {
            let last = self.order.len() - 1;
            self.cursor = Some(last);
            self.next_cycle = None;
            return PrevAction::Play(self.order[last]);
        }
        PrevAction::Restart(current)
    }

    /// Up to `n` items that will play after the current one (for "Up next").
    pub fn upcoming(&mut self, n: usize) -> Vec<usize> {
        let Some(cursor) = self.cursor else {
            return Vec::new();
        };
        let mut out: Vec<usize> = self.order[cursor + 1..].iter().take(n).copied().collect();
        if out.len() < n && self.repeat != RepeatMode::Off && self.len > 0 {
            let more = n - out.len();
            let cycle = self.next_cycle_order().to_vec();
            out.extend(cycle.iter().take(more));
        }
        out
    }

    fn step_forward(&mut self, wrap: bool) -> Option<usize> {
        let cursor = self.cursor?;
        if cursor + 1 < self.order.len() {
            self.cursor = Some(cursor + 1);
            return Some(self.order[cursor + 1]);
        }
        if !wrap {
            return None;
        }
        let next = self.next_cycle_order().to_vec();
        self.order = next;
        self.next_cycle = None;
        self.cursor = Some(0);
        Some(self.order[0])
    }

    /// Order for the next cycle: identity, or a fresh permutation that does not
    /// start with the item that just played.
    fn next_cycle_order(&mut self) -> &[usize] {
        if self.next_cycle.is_none() {
            let order = if self.shuffle {
                let last = self.current();
                let mut order: Vec<usize> = (0..self.len).collect();
                self.rng.shuffle(&mut order);
                if self.len > 1 && order.first().copied() == last {
                    let j = 1 + self.rng.below(self.len - 1);
                    order.swap(0, j);
                }
                order
            } else {
                (0..self.len).collect()
            };
            self.next_cycle = Some(order);
        }
        self.next_cycle.as_deref().unwrap_or(&[])
    }

    fn shuffled_starting_with(&mut self, first: usize) -> Vec<usize> {
        let mut rest: Vec<usize> = (0..self.len).filter(|&i| i != first).collect();
        self.rng.shuffle(&mut rest);
        let mut order = Vec::with_capacity(self.len);
        order.push(first);
        order.extend(rest);
        order
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn queue(len: usize, start: usize) -> Queue {
        let mut q = Queue::new(Rng::new(42));
        q.set_items(len, start);
        q
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn empty_queue_does_nothing() {
        let mut q = queue(0, 0);
        assert_eq!(q.current(), None);
        assert_eq!(q.advance_auto(), None);
        assert_eq!(q.next_manual(), None);
        assert_eq!(q.peek_next_auto(), None);
        assert_eq!(q.prev(secs(0)), PrevAction::Nothing);
    }

    #[test]
    fn plays_in_order_then_stops_with_repeat_off() {
        let mut q = queue(3, 0);
        assert_eq!(q.current(), Some(0));
        assert_eq!(q.peek_next_auto(), Some(1));
        assert_eq!(q.advance_auto(), Some(1));
        assert_eq!(q.advance_auto(), Some(2));
        assert_eq!(q.peek_next_auto(), None);
        assert_eq!(q.advance_auto(), None);
        assert_eq!(q.current(), Some(2), "cursor stays on the last track");
        assert_eq!(q.next_manual(), None);
    }

    #[test]
    fn start_index_is_clamped() {
        let q = queue(3, 10);
        assert_eq!(q.current(), Some(2));
    }

    #[test]
    fn repeat_all_wraps() {
        let mut q = queue(3, 2);
        q.set_repeat(RepeatMode::All);
        assert_eq!(q.peek_next_auto(), Some(0));
        assert_eq!(q.advance_auto(), Some(0));
        assert_eq!(q.next_manual(), Some(1));
    }

    #[test]
    fn repeat_one_repeats_on_auto_but_manual_next_moves() {
        let mut q = queue(3, 1);
        q.set_repeat(RepeatMode::One);
        assert_eq!(q.peek_next_auto(), Some(1));
        assert_eq!(q.advance_auto(), Some(1));
        assert_eq!(q.advance_auto(), Some(1));
        assert_eq!(q.next_manual(), Some(2));
        // Manual next at the end wraps under repeat one.
        assert_eq!(q.next_manual(), Some(0));
    }

    #[test]
    fn prev_restarts_after_three_seconds() {
        let mut q = queue(3, 1);
        assert_eq!(q.prev(secs(5)), PrevAction::Restart(1));
        assert_eq!(q.current(), Some(1));
        assert_eq!(q.prev(Duration::from_millis(3001)), PrevAction::Restart(1));
    }

    #[test]
    fn prev_goes_back_within_three_seconds() {
        let mut q = queue(3, 1);
        assert_eq!(q.prev(secs(3)), PrevAction::Play(0));
        assert_eq!(q.current(), Some(0));
        // At the first track with repeat off: restart.
        assert_eq!(q.prev(secs(1)), PrevAction::Restart(0));
    }

    #[test]
    fn prev_wraps_with_repeat_all() {
        let mut q = queue(3, 0);
        q.set_repeat(RepeatMode::All);
        assert_eq!(q.prev(secs(1)), PrevAction::Play(2));
    }

    #[test]
    fn shuffle_keeps_current_first_and_covers_everything() {
        let mut q = queue(50, 17);
        q.set_shuffle(true);
        assert_eq!(q.current(), Some(17));
        let mut seen = HashSet::new();
        seen.insert(17);
        while let Some(i) = q.advance_auto() {
            assert!(seen.insert(i), "item {i} repeated before the cycle ended");
        }
        assert_eq!(seen.len(), 50);
    }

    #[test]
    fn shuffle_order_is_not_identity() {
        let mut q = queue(50, 0);
        q.set_shuffle(true);
        let order = q.order().to_vec();
        assert_ne!(order, (0..50).collect::<Vec<_>>());
    }

    #[test]
    fn shuffle_repeat_all_reshuffles_each_cycle_without_repeats() {
        let mut q = queue(20, 0);
        q.set_shuffle(true);
        q.set_repeat(RepeatMode::All);
        let mut cycles: Vec<Vec<usize>> = Vec::new();
        let mut current = vec![q.current().unwrap_or(0)];
        for _ in 0..(20 * 4 - 1) {
            let next = q.advance_auto().unwrap_or(usize::MAX);
            if current.len() == 20 {
                // The first item of a new cycle never repeats the item just played.
                assert_ne!(Some(&next), current.last());
                cycles.push(std::mem::take(&mut current));
            }
            current.push(next);
        }
        cycles.push(current);
        assert_eq!(cycles.len(), 4);
        for cycle in &cycles {
            let set: HashSet<_> = cycle.iter().collect();
            assert_eq!(set.len(), 20, "each cycle plays every item exactly once");
        }
        assert_ne!(cycles[1], cycles[2], "cycles are reshuffled");
    }

    #[test]
    fn peek_matches_advance_across_cycle_boundary() {
        let mut q = queue(5, 0);
        q.set_shuffle(true);
        q.set_repeat(RepeatMode::All);
        for _ in 0..23 {
            let peeked = q.peek_next_auto();
            assert_eq!(q.advance_auto(), peeked);
        }
    }

    #[test]
    fn turning_shuffle_off_restores_order_at_current_item() {
        let mut q = queue(10, 0);
        q.set_shuffle(true);
        q.advance_auto();
        q.advance_auto();
        let current = q.current();
        q.set_shuffle(false);
        assert_eq!(q.current(), current);
        assert_eq!(q.order(), (0..10).collect::<Vec<_>>().as_slice());
        let c = current.unwrap_or(0);
        assert_eq!(
            q.peek_next_auto(),
            if c + 1 < 10 { Some(c + 1) } else { None }
        );
    }

    #[test]
    fn jump_to_with_shuffle_starts_a_new_cycle() {
        let mut q = queue(10, 0);
        q.set_shuffle(true);
        assert_eq!(q.jump_to(7), Some(7));
        assert_eq!(q.current(), Some(7));
        let mut seen = HashSet::from([7]);
        while let Some(i) = q.advance_auto() {
            assert!(seen.insert(i));
        }
        assert_eq!(seen.len(), 10);
        assert_eq!(q.jump_to(10), None);
    }

    #[test]
    fn single_item_shuffle_repeat_all() {
        let mut q = queue(1, 0);
        q.set_shuffle(true);
        q.set_repeat(RepeatMode::All);
        assert_eq!(q.advance_auto(), Some(0));
        assert_eq!(q.advance_auto(), Some(0));
    }

    #[test]
    fn upcoming_lists_next_items_and_wraps_with_repeat() {
        let mut q = queue(4, 2);
        assert_eq!(q.upcoming(3), vec![3]);
        q.set_repeat(RepeatMode::All);
        assert_eq!(q.upcoming(3), vec![3, 0, 1]);
        // Upcoming agrees with what actually plays.
        assert_eq!(q.advance_auto(), Some(3));
        assert_eq!(q.advance_auto(), Some(0));
    }

    #[test]
    fn rng_below_is_in_range() {
        let mut r = Rng::new(1);
        for n in 1..100 {
            assert!(r.below(n) < n);
        }
    }

    #[test]
    fn insert_play_next_and_append() {
        let mut q = queue(4, 1);
        q.insert(2, true); // items 4, 5 play right after item 1
        assert_eq!(q.order(), [0, 1, 4, 5, 2, 3]);
        assert_eq!(q.current(), Some(1));
        assert_eq!(q.peek_next_auto(), Some(4));
        q.insert(1, false); // item 6 at the end
        assert_eq!(q.order(), [0, 1, 4, 5, 2, 3, 6]);
        assert_eq!(q.advance_auto(), Some(4));
        assert_eq!(q.advance_auto(), Some(5));
        assert_eq!(q.advance_auto(), Some(2));
        // Picking an item finds it in the modified order.
        assert_eq!(q.jump_to(6), Some(6));
        assert_eq!(q.current(), Some(6));
        assert_eq!(q.advance_auto(), None);
        assert_eq!(q.jump_to(5), Some(5));
        assert_eq!(q.upcoming(2), vec![2, 3]);

        // Shuffled: "play next" still comes next, and the cycle covers everything.
        let mut q = queue(5, 0);
        q.set_shuffle(true);
        let cur = q.current();
        q.insert(1, true);
        assert_eq!(q.current(), cur);
        assert_eq!(q.peek_next_auto(), Some(5));
        let mut seen: Vec<usize> = q.order().to_vec();
        seen.sort_unstable();
        assert_eq!(seen, (0..6).collect::<Vec<_>>());

        // An empty queue gets a current item.
        let mut q = queue(0, 0);
        q.insert(2, false);
        assert_eq!(q.current(), Some(0));
        assert_eq!(q.len(), 2);
    }
}
