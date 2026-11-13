//! The user-facing loss guarantee, stated directly.
//!
//! The shadow model (`model.rs`) restates the contract's rules, including the
//! sliding window's slot arithmetic, so it agrees with the contract by
//! construction. This module is a different kind of witness. It keeps only a
//! list of what was actually booked, each spend with the time it was booked, and
//! asserts, from that list alone, what the owner was promised:
//!
//! 1. **Window.** Spend booked inside any real interval of `WINDOW_LEN`
//!    nanoseconds is at most the asset's `window_limit`.
//! 2. **Budget.** Spend booked under one mandate epoch is at most that asset's
//!    `epoch_budget`.
//! 3. **Worst-case loss.** Spend booked under one epoch is at most
//!    `min(sum of the windows the epoch spans, epoch_budget)`.
//!
//! It uses no slot, bucket or ring: only times, amounts and the limits that were
//! in force when each spend was requested.
//!
//! **Which limit.** A spend is checked against the limit in force when it was
//! *requested*, because that is when the contract decides, and a request can
//! settle much later (it is reserved while in flight) and the owner may change a
//! limit in between. For a set of spends booked together the bound is the
//! largest limit among their requests: the last-requested of them was allowed
//! with every other one either already booked inside the window or still
//! reserved, so their sum was at most the limit it was checked against. A limit
//! the owner lowers below what was already counted therefore needs no special
//! case here: the lower limit binds only requests made after it.
//!
//! **Intervals.** "Within a window" means a half-open interval of `WINDOW_LEN`
//! nanoseconds, `[x, x + WINDOW_LEN)`: spends at `x` and at `x + WINDOW_LEN` are a
//! full window apart and are not in one. The sum over such an interval is largest
//! when its last nanosecond is a booking time, so it is enough, and exact, to
//! take each booking time `T` and sum the spends booked at `t` with
//! `T - WINDOW_LEN < t <= T`.

use super::actions::ASSET_COUNT;
use super::WINDOW_LEN;

/// One spend, as booked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Booking {
    /// Block time at which the spend was booked (settlement time, not request time).
    pub time: u64,
    pub amount: u128,
    /// The asset's `window_limit` when the spend was requested.
    pub limit: u128,
    /// The asset's `epoch_budget` when the spend was requested.
    pub budget: Option<u128>,
}

/// What one mandate epoch has booked of one asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tally {
    epoch: u64,
    total: u128,
    first: u64,
    last: u64,
    /// The largest `window_limit` of any request booked in the epoch.
    max_limit: u128,
    /// The largest `epoch_budget` of any request booked in the epoch, or `None`
    /// if any of them was requested with no budget (then no budget bound holds).
    budget_bound: Option<u128>,
}

#[derive(Clone, Debug, Default)]
pub struct Ledger {
    /// Bookings that can still be in a future window, per asset, in time order.
    recent: [Vec<Booking>; ASSET_COUNT],
    /// The current epoch's tally per asset. An epoch's bookings all arrive while
    /// it is current, so an older epoch's tally is final and was already checked.
    epoch: [Option<Tally>; ASSET_COUNT],
}

impl Ledger {
    /// Records a spend booked at `booking.time` under mandate `epoch`.
    pub fn book(&mut self, asset: usize, epoch: u64, booking: Booking) {
        self.recent[asset].push(booking);
        let tally = match self.epoch[asset] {
            Some(tally) if tally.epoch == epoch => Tally {
                total: tally.total.saturating_add(booking.amount),
                last: booking.time,
                max_limit: tally.max_limit.max(booking.limit),
                budget_bound: match (tally.budget_bound, booking.budget) {
                    (Some(bound), Some(budget)) => Some(bound.max(budget)),
                    _ => None,
                },
                ..tally
            },
            _ => Tally {
                epoch,
                total: booking.amount,
                first: booking.time,
                last: booking.time,
                max_limit: booking.limit,
                budget_bound: booking.budget,
            },
        };
        self.epoch[asset] = Some(tally);
    }

    /// Drops bookings too old to be in any window ending at `now` or later. Call
    /// after `check`.
    pub fn prune(&mut self, now: u64) {
        for bookings in &mut self.recent {
            bookings.retain(|booking| booking.time.saturating_add(WINDOW_LEN) > now);
        }
    }

    /// Asserts the three guarantees over what has been booked.
    pub fn check(&self) -> Result<(), String> {
        for asset in 0..ASSET_COUNT {
            check_windows(asset, &self.recent[asset])?;
            if let Some(tally) = &self.epoch[asset] {
                check_epoch(asset, tally)?;
            }
        }
        Ok(())
    }
}

/// The largest sum over any interval of `WINDOW_LEN`, against its bound. The
/// bookings are in time order.
fn check_windows(asset: usize, bookings: &[Booking]) -> Result<(), String> {
    for (i, anchor) in bookings.iter().enumerate() {
        let end = anchor.time;
        let mut sum = 0u128;
        let mut bound = 0u128;
        // Every booking at `end` or earlier, and no more than a window before it.
        // Ties after `anchor` in the list are at the same time, so they count.
        for member in bookings {
            if member.time <= end && member.time.saturating_add(WINDOW_LEN) > end {
                sum = sum.saturating_add(member.amount);
                bound = bound.max(member.limit);
            }
        }
        if sum > bound {
            return Err(format!(
                "asset {asset}: {sum} was booked within one window ending at {end}, \
                 over the largest limit ({bound}) of the requests that booked it \
                 (booking {i} of {})",
                bookings.len()
            ));
        }
    }
    Ok(())
}

fn check_epoch(asset: usize, tally: &Tally) -> Result<(), String> {
    if let Some(budget) = tally.budget_bound {
        if tally.total > budget {
            return Err(format!(
                "asset {asset}: {} was booked under mandate epoch {}, over its budget of {budget}",
                tally.total, tally.epoch
            ));
        }
    }
    // The windows the epoch's bookings span: consecutive intervals of
    // `WINDOW_LEN` from the first booking cover every booking up to the last.
    let windows = u128::from((tally.last - tally.first) / WINDOW_LEN) + 1;
    let by_windows = tally.max_limit.saturating_mul(windows);
    let worst_case = match tally.budget_bound {
        Some(budget) => by_windows.min(budget),
        None => by_windows,
    };
    if tally.total > worst_case {
        return Err(format!(
            "asset {asset}: {} was booked under mandate epoch {} across {windows} window(s), \
             over min(windows * limit, budget) = {worst_case}",
            tally.total, tally.epoch
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! The checker checked: hand-made histories, so that a bound this module
    //! enforces is shown to bind and one it must not enforce is shown not to.

    use super::*;

    const LIMIT: u128 = 100;

    fn booking(time: u64, amount: u128) -> Booking {
        Booking {
            time,
            amount,
            limit: LIMIT,
            budget: None,
        }
    }

    fn ledger(bookings: &[Booking]) -> Ledger {
        let mut ledger = Ledger::default();
        for booking in bookings {
            ledger.book(0, 1, *booking);
        }
        ledger
    }

    #[test]
    fn spend_exactly_a_window_apart_is_in_different_windows() {
        // The first stops counting a full window later, so each interval holds one.
        assert!(ledger(&[booking(0, 100), booking(WINDOW_LEN, 100)])
            .check()
            .is_ok());
    }

    #[test]
    fn spend_one_nanosecond_short_of_a_window_apart_is_in_one_window() {
        let history = ledger(&[booking(0, 100), booking(WINDOW_LEN - 1, 1)]);
        let error = history.check().expect_err("101 inside one window");
        assert!(error.contains("101"), "{error}");
    }

    #[test]
    fn the_sum_is_over_every_booking_in_the_interval_not_only_neighbours() {
        let history = ledger(&[
            booking(0, 40),
            booking(100, 40),
            booking(200, 40),
            booking(300, 40),
        ]);
        assert!(history.check().is_err(), "160 inside 300 ns");
    }

    #[test]
    fn spend_at_the_same_instant_counts_together() {
        assert!(ledger(&[booking(5, 60), booking(5, 41)]).check().is_err());
        assert!(ledger(&[booking(5, 60), booking(5, 40)]).check().is_ok());
    }

    #[test]
    fn a_long_run_that_never_exceeds_the_limit_in_any_window_passes() {
        // 100 every window, forever: the worst-case-loss bound grows with the
        // windows spanned, so this is allowed.
        let history: Vec<Booking> = (0..20).map(|k| booking(k * WINDOW_LEN, 100)).collect();
        assert!(ledger(&history).check().is_ok());
    }

    #[test]
    fn the_bound_is_the_largest_limit_among_the_requests_that_booked_it() {
        // 150 booked together; one request was made when the limit was 160.
        let loose = Booking {
            limit: 160,
            ..booking(10, 100)
        };
        assert!(ledger(&[booking(0, 50), loose]).check().is_ok());
        // Without that request the same sum is over every limit.
        assert!(ledger(&[booking(0, 50), booking(10, 100)]).check().is_err());
    }

    #[test]
    fn the_budget_bounds_one_epoch_and_a_new_epoch_starts_afresh() {
        let capped = |time, amount| Booking {
            budget: Some(150),
            ..booking(time, amount)
        };
        let mut history = Ledger::default();
        history.book(0, 1, capped(0, 100));
        history.book(0, 1, capped(WINDOW_LEN, 100));
        let error = history.check().expect_err("200 under a budget of 150");
        assert!(error.contains("budget of 150"), "{error}");

        let mut regranted = Ledger::default();
        regranted.book(0, 1, capped(0, 100));
        regranted.book(0, 2, capped(WINDOW_LEN, 100));
        assert!(regranted.check().is_ok(), "a new epoch resets the budget");
    }

    #[test]
    fn a_request_made_without_a_budget_lifts_the_budget_bound() {
        let mut history = Ledger::default();
        history.book(
            0,
            1,
            Booking {
                budget: Some(150),
                ..booking(0, 100)
            },
        );
        history.book(0, 1, booking(WINDOW_LEN, 100));
        assert!(history.check().is_ok());
    }

    #[test]
    fn the_worst_case_loss_is_windows_spanned_times_the_limit() {
        // Three windows can carry 300, never 400, even spread out.
        let three = [
            booking(0, 100),
            booking(WINDOW_LEN, 100),
            booking(2 * WINDOW_LEN, 100),
        ];
        assert!(ledger(&three).check().is_ok());
        // 100 + 100 + 100 + 100 inside 3 windows would already break the window
        // rule; the loss bound is exercised through the epoch tally directly.
        let mut over = ledger(&three);
        over.epoch[0].as_mut().expect("a tally").total = 400;
        assert!(over.check().is_err());
    }

    #[test]
    fn pruning_forgets_only_what_can_no_longer_share_a_window() {
        let mut history = ledger(&[booking(0, 100), booking(WINDOW_LEN - 1, 0)]);
        history.prune(WINDOW_LEN - 1);
        assert_eq!(history.recent[0].len(), 2, "still a window apart at most");
        history.prune(WINDOW_LEN);
        assert_eq!(history.recent[0].len(), 1, "the first is a full window old");
    }

    #[test]
    fn a_separate_asset_has_its_own_history() {
        let mut history = Ledger::default();
        history.book(0, 1, booking(0, 100));
        history.book(1, 1, booking(1, 100));
        assert!(history.check().is_ok());
    }

    // ---- The real contract, under an adversary that watches the clock ----

    use std::panic::{catch_unwind, AssertUnwindSafe};

    use near_sdk::json_types::U128;
    use near_sdk::{testing_env, PromiseResult, RuntimeFeesConfig};

    use crate::test_support::{account, agent_context, armed_as_agent, context};
    use crate::AssetId;

    /// A greedy agent against the real contract. It asks for the per-transaction
    /// cap at times chosen to land on every offset within a slot, for many
    /// windows, and each request that is allowed settles at once. The record of
    /// what was booked, and nothing else, is then held to the three guarantees.
    ///
    /// This is the check that does not lean on the ring model: the ledger sees
    /// only times and amounts. A window that forgot a spend too early (an
    /// under-count) lets this agent book more than the limit within one real
    /// interval, and `check` says so in the owner's terms.
    #[test]
    fn a_greedy_agent_timing_its_requests_cannot_beat_the_limit_in_any_window() {
        const CAP: u128 = 50;
        const START: u64 = 1_000_000;
        let mut minder = armed_as_agent();
        let mut ledger = Ledger::default();
        let mut booked = 0u128;

        let mut offset = 0;
        while offset < 8 * WINDOW_LEN {
            let now = START + offset;
            let mut agent = agent_context();
            agent.block_timestamp(now);
            testing_env!(agent.build());
            let allowed = catch_unwind(AssertUnwindSafe(|| {
                let _ =
                    minder.request_local_transfer(AssetId::Near, U128(CAP), account("bob.near"));
            }))
            .is_ok();
            if allowed {
                let mut settle = context("minder.near");
                settle.block_timestamp(now);
                testing_env!(
                    settle.build(),
                    near_sdk::test_vm_config(),
                    RuntimeFeesConfig::test(),
                    Default::default(),
                    vec![PromiseResult::Successful(Vec::new())]
                );
                minder.on_transfer_complete(AssetId::Near, U128(CAP));
                ledger.book(
                    0,
                    1,
                    Booking {
                        time: now,
                        amount: CAP,
                        limit: crate::test_support::WINDOW_LIMIT,
                        budget: None,
                    },
                );
                booked += CAP;
            }
            if let Err(error) = ledger.check() {
                panic!("the window let through too much: {error}");
            }
            ledger.prune(now);
            offset += 7;
        }
        // It did get paid, over many windows, and never more than the limit per
        // window: 8 windows can carry at most 8 times the limit.
        assert!(booked >= 6 * CAP, "the agent was never allowed: {booked}");
        assert!(booked <= 8 * crate::test_support::WINDOW_LIMIT, "{booked}");
    }
}
