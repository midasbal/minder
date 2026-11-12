//! Unit tests for the per-grant lifetime budget (`AssetPolicy::epoch_budget`)
//! under the mocked NEAR environment: what counts against it, that a new grant
//! resets it lazily, and how it sits next to the window.

use std::panic::{catch_unwind, AssertUnwindSafe};

use near_sdk::json_types::{U128, U64};
use near_sdk::{testing_env, PromiseResult, RuntimeFeesConfig};

use crate::test_support::*;
use crate::{AssetId, Decision, DenyReason, EpochSpend, Minder};

const BUDGET: u128 = 70;
const FAR_FUTURE: u64 = u64::MAX;

thread_local! {
    /// The block time of the test, which `after_the_window` moves on. Every
    /// call below is made at this time, so settlements land where they should.
    static CLOCK: std::cell::Cell<u64> = const { std::cell::Cell::new(1_000_000) };
}

fn now() -> u64 {
    CLOCK.with(|clock| clock.get())
}

/// The agent's call at the test clock.
fn as_agent(key: near_sdk::PublicKey) {
    let mut context = agent_context_for(key);
    context.block_timestamp(now());
    testing_env!(context.build());
}

/// Armed as in the other tests (`PER_TX_CAP` 50, `WINDOW_LIMIT` 100, a 400 ns
/// window), with the NEAR policy replaced by one carrying `budget`. The
/// environment is left as the agent.
fn armed_with_budget(budget: Option<u128>) -> Minder {
    let mut minder = armed_as_agent();
    set_budget(&mut minder, budget);
    minder
}

/// Replaces the NEAR policy with the same limits and `budget`, as the owner.
fn set_budget(minder: &mut Minder, budget: Option<u128>) {
    as_owner();
    minder.set_asset_policy(
        AssetId::Near,
        U128(PER_TX_CAP),
        U128(WINDOW_LIMIT),
        budget.map(U128),
    );
    as_agent(agent_key());
}

fn request(minder: &mut Minder, amount: u128) {
    let _ = minder.request_local_transfer(AssetId::Near, U128(amount), account("bob.near"));
}

/// Whether a real request is accepted, or the reason it is denied for. The
/// view must say the same.
fn attempt(minder: &mut Minder, key: near_sdk::PublicKey, amount: u128) -> Decision {
    let viewed = minder.dry_run(key, AssetId::Near, U128(amount), account("bob.near"));
    let result = catch_unwind(AssertUnwindSafe(|| request(minder, amount)));
    let actual = match result {
        Ok(()) => Decision::Allow,
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_default();
            let reason = [
                DenyReason::WindowLimitExceeded,
                DenyReason::MandateBudgetExceeded,
                DenyReason::PerTxCapExceeded,
            ]
            .into_iter()
            .find(|reason| message.contains(&format!("Denied: {reason:?}")))
            .unwrap_or_else(|| panic!("unexpected failure: {message}"));
            Decision::Deny(reason)
        }
    };
    assert_eq!(viewed, actual, "the view must agree with the request");
    actual
}

fn ask(minder: &mut Minder, amount: u128) -> Decision {
    attempt(minder, agent_key(), amount)
}

/// Settles a request of `amount` (success unless `result` says otherwise) and
/// returns the environment to `key`'s calls.
fn settle(minder: &mut Minder, amount: u128, result: PromiseResult, key: near_sdk::PublicKey) {
    let mut callback = context("minder.near");
    callback.block_timestamp(now());
    testing_env!(
        callback.build(),
        near_sdk::test_vm_config(),
        RuntimeFeesConfig::test(),
        Default::default(),
        vec![result]
    );
    minder.on_transfer_complete(AssetId::Near, U128(amount));
    as_agent(key);
}

fn settle_ok(minder: &mut Minder, amount: u128) {
    settle(
        minder,
        amount,
        PromiseResult::Successful(Vec::new()),
        agent_key(),
    );
}

/// A request that is allowed and settles successfully.
fn spend(minder: &mut Minder, amount: u128) {
    assert_eq!(ask(minder, amount), Decision::Allow);
    settle_ok(minder, amount);
}

/// Moves the clock a long way on, so the window has emptied.
fn after_the_window(key: near_sdk::PublicKey) {
    CLOCK.with(|clock| clock.set(clock.get() + 10_000));
    as_agent(key);
}

fn as_owner() {
    let mut context = context("owner.near");
    context.block_timestamp(now());
    testing_env!(context.build());
}

#[test]
fn the_budget_denies_when_committed_spend_plus_the_request_would_exceed_it() {
    let mut minder = armed_with_budget(Some(BUDGET));
    spend(&mut minder, 50);
    spend(&mut minder, 20); // 70: exactly the budget is allowed
    assert_eq!(
        ask(&mut minder, 1),
        Decision::Deny(DenyReason::MandateBudgetExceeded)
    );
    // The window (100) still has room, so it is the budget that said no.
    assert_eq!(committed(&minder, AssetId::Near), 70);
}

#[test]
fn in_flight_requests_count_against_the_budget_before_they_settle() {
    let mut minder = armed_with_budget(Some(BUDGET));
    assert_eq!(ask(&mut minder, 50), Decision::Allow);
    assert_eq!(reserved(&minder, AssetId::Near), 50);
    assert_eq!(committed(&minder, AssetId::Near), 0);
    // 50 reserved + 30 = 80 > 70, though the window (100) has room and nothing
    // has settled: the reserved counter is what the budget reads.
    assert_eq!(
        ask(&mut minder, 30),
        Decision::Deny(DenyReason::MandateBudgetExceeded)
    );
    assert_eq!(ask(&mut minder, 20), Decision::Allow);
}

#[test]
fn a_failed_transfer_gives_its_budget_back() {
    let mut minder = armed_with_budget(Some(BUDGET));
    assert_eq!(ask(&mut minder, 50), Decision::Allow);
    settle(&mut minder, 50, PromiseResult::Failed, agent_key());
    assert_eq!(minder.epoch_committed(&AssetId::Near), 0);
    spend(&mut minder, 50);
    spend(&mut minder, 20);
}

#[test]
fn a_new_grant_resets_the_budget_without_writing_anything() {
    let mut minder = armed_with_budget(Some(BUDGET));
    spend(&mut minder, 50);
    spend(&mut minder, 20);
    assert_eq!(
        ask(&mut minder, 1),
        Decision::Deny(DenyReason::MandateBudgetExceeded)
    );

    as_owner();
    minder.revoke_mandate();
    minder.grant_mandate(other_key(), U64(FAR_FUTURE), None);
    as_agent(other_key());

    // The window is global and a grant does not reset it, so move past it:
    // what is left to examine is the budget alone.
    after_the_window(other_key());
    // The counter in storage is untouched: still the old epoch's 70 ...
    assert_eq!(
        minder.epoch_spend.get(&AssetId::Near).copied(),
        Some(EpochSpend {
            epoch: 1,
            committed: 70
        })
    );
    // ... but it belongs to epoch 1 and the mandate is epoch 2, so it reads as zero.
    assert_eq!(minder.epoch_committed(&AssetId::Near), 0);
    assert_eq!(attempt(&mut minder, other_key(), 50), Decision::Allow);
    settle(
        &mut minder,
        50,
        PromiseResult::Successful(Vec::new()),
        other_key(),
    );
    assert_eq!(
        minder.epoch_spend.get(&AssetId::Near).copied(),
        Some(EpochSpend {
            epoch: 2,
            committed: 50
        })
    );
    assert_eq!(attempt(&mut minder, other_key(), 20), Decision::Allow);
    // A fresh 70 and no more.
    settle(
        &mut minder,
        20,
        PromiseResult::Successful(Vec::new()),
        other_key(),
    );
    assert_eq!(
        attempt(&mut minder, other_key(), 1),
        Decision::Deny(DenyReason::MandateBudgetExceeded)
    );
}

#[test]
fn spend_settled_after_a_regrant_is_charged_to_the_new_grant() {
    let mut minder = armed_with_budget(Some(BUDGET));
    assert_eq!(ask(&mut minder, 40), Decision::Allow); // in flight under epoch 1

    as_owner();
    minder.revoke_mandate();
    minder.grant_mandate(other_key(), U64(FAR_FUTURE), None);
    as_agent(other_key());

    // The in-flight 40 is still counted (reserved) against the new grant ...
    assert_eq!(
        attempt(&mut minder, other_key(), 40),
        Decision::Deny(DenyReason::MandateBudgetExceeded)
    );
    // ... and when it settles it is booked to the new grant, never dropped.
    settle(
        &mut minder,
        40,
        PromiseResult::Successful(Vec::new()),
        other_key(),
    );
    assert_eq!(minder.epoch_committed(&AssetId::Near), 40);
    assert_eq!(reserved(&minder, AssetId::Near), 0);
}

#[test]
fn the_budget_binds_before_the_window_when_it_is_the_smaller() {
    let mut minder = armed_with_budget(Some(BUDGET)); // window 100
    spend(&mut minder, 50);
    assert_eq!(
        ask(&mut minder, 30),
        Decision::Deny(DenyReason::MandateBudgetExceeded),
        "the window would allow 50 + 30"
    );
}

#[test]
fn the_window_binds_before_the_budget_and_the_budget_outlasts_the_window() {
    let mut minder = armed_with_budget(Some(150)); // window 100
    spend(&mut minder, 50);
    spend(&mut minder, 50);
    assert_eq!(
        ask(&mut minder, 50),
        Decision::Deny(DenyReason::WindowLimitExceeded),
        "the budget has room, the window does not"
    );

    // Once the window has emptied the window allows again; the budget does not decay.
    after_the_window(agent_key());
    spend(&mut minder, 50); // 150 committed in this grant
    assert_eq!(
        ask(&mut minder, 1),
        Decision::Deny(DenyReason::MandateBudgetExceeded),
        "the window holds only 50 now, but the grant has spent its budget"
    );
}

#[test]
fn where_both_would_deny_the_window_is_the_reason() {
    let mut minder = armed_with_budget(Some(BUDGET));
    spend(&mut minder, 50);
    spend(&mut minder, 20);
    // 70 committed: 50 more is 120, over the budget (70) and the window (100).
    assert_eq!(
        ask(&mut minder, 50),
        Decision::Deny(DenyReason::WindowLimitExceeded)
    );
}

#[test]
fn an_unset_budget_is_the_window_alone() {
    let mut minder = armed_with_budget(None);
    spend(&mut minder, 50);
    spend(&mut minder, 50);
    assert_eq!(
        ask(&mut minder, 1),
        Decision::Deny(DenyReason::WindowLimitExceeded)
    );
    // Over many windows the total is not bounded by anything but the window.
    for _ in 0..5 {
        after_the_window(agent_key());
        spend(&mut minder, 50);
        spend(&mut minder, 50);
    }
    assert!(minder.epoch_committed(&AssetId::Near) >= 600);
}

#[test]
fn a_budget_set_mid_grant_counts_what_the_grant_already_spent() {
    let mut minder = armed_with_budget(None);
    spend(&mut minder, 50);
    spend(&mut minder, 10);
    set_budget(&mut minder, Some(BUDGET));
    assert_eq!(
        ask(&mut minder, 20),
        Decision::Deny(DenyReason::MandateBudgetExceeded)
    );
    assert_eq!(ask(&mut minder, 10), Decision::Allow);
}

#[test]
fn lowering_the_budget_below_what_was_used_denies_until_the_next_grant() {
    let mut minder = armed_with_budget(Some(BUDGET));
    spend(&mut minder, 50);
    set_budget(&mut minder, Some(40));
    assert_eq!(
        ask(&mut minder, 1),
        Decision::Deny(DenyReason::MandateBudgetExceeded)
    );
}

#[test]
fn removing_and_re_adding_a_policy_does_not_reset_the_budget() {
    let mut minder = armed_with_budget(Some(BUDGET));
    spend(&mut minder, 50);
    as_owner();
    minder.remove_asset_policy(AssetId::Near);
    as_agent(agent_key());
    set_budget(&mut minder, Some(BUDGET));
    assert_eq!(
        ask(&mut minder, 30),
        Decision::Deny(DenyReason::MandateBudgetExceeded)
    );
}

#[test]
fn the_budget_is_per_asset() {
    let mut minder = armed_with_budget(Some(BUDGET));
    spend(&mut minder, 50);
    // The token has no budget and its own spend: the NEAR budget is not touched by it.
    let viewed = minder.dry_run(agent_key(), token(), U128(50), account("bob.near"));
    assert_eq!(viewed, Decision::Allow);
}
