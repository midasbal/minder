//! Sliding spend window for a `k`-slot window: a ring of `k + 1` buckets.
//!
//! Time is divided into slots of `slot_len = window_len / k` nanoseconds.
//! The slot index of a timestamp is `now / slot_len`. Each bucket records the
//! slot index it last accumulated spend in, plus that amount. A bucket is live
//! while `current_slot - bucket.slot <= k`, so spend counts for `k + 1` slots:
//! the slot it was made in plus the `k` after it.
//!
//! Spend made at `t` is therefore forgotten between `t + window_len` and
//! `t + window_len + slot_len`: never early, at most one slot width late. The
//! window never under-enforces its limit and over-enforces by at most one slot
//! width. The extra bucket (`k + 1` rather than `k`) is what lets the oldest
//! live slot, age `k`, coexist with the current slot without sharing a ring
//! index.

use near_sdk::near;

#[near(serializers = [borsh])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bucket {
    slot: u64,
    amount: u128,
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpendRing {
    buckets: Vec<Bucket>,
}

/// Slot index of a timestamp.
fn slot_of(now: u64, slot_len: u64) -> u64 {
    now / slot_len
}

impl SpendRing {
    /// `num_buckets` is the user-facing `k`; the ring holds one more.
    pub fn new(num_buckets: u32) -> Self {
        let empty = Bucket { slot: 0, amount: 0 };
        Self {
            buckets: vec![empty; num_buckets as usize + 1],
        }
    }

    /// The user-facing `k`: how many slots a bucket outlives its own.
    fn num_buckets(&self) -> u64 {
        self.buckets.len() as u64 - 1
    }

    fn is_live(&self, bucket: &Bucket, current_slot: u64) -> bool {
        current_slot
            .checked_sub(bucket.slot)
            .is_some_and(|age| age <= self.num_buckets())
    }

    /// Total spend in live buckets at time `now`. Saturates rather than
    /// wraps: a saturated sum exceeds every limit, so the result stays safe
    /// (deny) instead of silently under-counting.
    pub fn live_sum(&self, now: u64, slot_len: u64) -> u128 {
        let current_slot = slot_of(now, slot_len);
        self.buckets
            .iter()
            .filter(|bucket| self.is_live(bucket, current_slot))
            .fold(0u128, |total, bucket| total.saturating_add(bucket.amount))
    }

    /// Adds `amount` to the bucket for the slot containing `now`, first
    /// zeroing it if it still holds spend from an older slot (the ring
    /// "advance"). A bucket is only reused once its old slot is past `k`, so
    /// nothing live is ever overwritten.
    pub fn record(&mut self, now: u64, slot_len: u64, amount: u128) {
        let current_slot = slot_of(now, slot_len);
        let index = (current_slot % self.buckets.len() as u64) as usize;
        let bucket = &mut self.buckets[index];
        if bucket.slot != current_slot {
            *bucket = Bucket {
                slot: current_slot,
                amount: 0,
            };
        }
        bucket.amount = bucket.amount.saturating_add(amount);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOT: u64 = 100;

    #[test]
    fn spend_is_live_for_k_plus_one_slots() {
        let mut ring = SpendRing::new(4);
        ring.record(250, SLOT, 7); // slot 2
        assert_eq!(ring.live_sum(250, SLOT), 7);
        assert_eq!(ring.live_sum(699, SLOT), 7); // slot 6 = 2 + k: last live slot
        assert_eq!(ring.live_sum(700, SLOT), 0); // slot 7 = 2 + k + 1: expired
    }

    #[test]
    fn spend_is_never_forgotten_early_and_at_most_one_slot_late() {
        // Spend recorded at t is forgotten no earlier than t + window and no
        // later than t + window + slot, where window = k * SLOT.
        let k = 4u64;
        let window = k * SLOT;
        for t in 0..(3 * window) {
            let mut ring = SpendRing::new(k as u32);
            ring.record(t, SLOT, 1);
            let forgotten_at = (t..t + 2 * window)
                .find(|&now| ring.live_sum(now, SLOT) == 0)
                .expect("spend must expire");
            assert!(forgotten_at >= t + window, "forgotten early at t={t}");
            assert!(
                forgotten_at <= t + window + SLOT,
                "forgotten more than a slot late at t={t}"
            );
        }
    }

    #[test]
    fn single_bucket_window_counts_for_two_slots() {
        let mut ring = SpendRing::new(1);
        ring.record(50, SLOT, 5); // slot 0
        assert_eq!(ring.live_sum(199, SLOT), 5); // slot 1: still live
        assert_eq!(ring.live_sum(200, SLOT), 0); // slot 2: expired
    }

    #[test]
    fn reused_bucket_is_zeroed_before_adding() {
        let mut ring = SpendRing::new(2); // 3 buckets
        ring.record(0, SLOT, 10); // slot 0 -> index 0
        ring.record(300, SLOT, 3); // slot 3 -> index 0 again; the 10 is expired
        assert_eq!(ring.live_sum(300, SLOT), 3);
    }

    #[test]
    fn oldest_live_slot_does_not_alias_the_current_slot() {
        let mut ring = SpendRing::new(2);
        ring.record(0, SLOT, 10); // slot 0
        ring.record(200, SLOT, 3); // slot 2 = 0 + k: oldest bucket still live
        assert_eq!(ring.live_sum(200, SLOT), 13);
        ring.record(300, SLOT, 1); // slot 3: only now may slot 0's bucket be reused
        assert_eq!(ring.live_sum(300, SLOT), 4);
    }

    #[test]
    fn live_sum_adds_across_buckets_and_ages_them_out_one_at_a_time() {
        let mut ring = SpendRing::new(3);
        ring.record(0, SLOT, 1);
        ring.record(100, SLOT, 2);
        ring.record(200, SLOT, 4);
        assert_eq!(ring.live_sum(200, SLOT), 7);
        assert_eq!(ring.live_sum(300, SLOT), 7); // slot 0 is age k: still live
        assert_eq!(ring.live_sum(400, SLOT), 6); // slot 0 aged out
        assert_eq!(ring.live_sum(500, SLOT), 4); // slot 1 aged out
        assert_eq!(ring.live_sum(600, SLOT), 0); // slot 2 aged out
    }

    #[test]
    fn spend_in_one_slot_accumulates() {
        let mut ring = SpendRing::new(4);
        ring.record(10, SLOT, 3);
        ring.record(90, SLOT, 4);
        assert_eq!(ring.live_sum(90, SLOT), 7);
    }

    #[test]
    fn sum_saturates_instead_of_wrapping() {
        let mut ring = SpendRing::new(2);
        ring.record(0, SLOT, u128::MAX);
        ring.record(100, SLOT, 1);
        assert_eq!(ring.live_sum(100, SLOT), u128::MAX);
    }
}
