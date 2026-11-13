//! Unit tests for the lifetime budget (`AssetPolicy::epoch_budget`) under the
//! mocked NEAR environment: what counts against it, that it belongs to a budget
//! period apart from the agent's key epoch (a re-grant carries usage forward; a
//! deliberate reset starts a fresh period, timelocked when anything is spent),
//! and how it sits next to the window.

use std::panic::{catch_unwind, AssertUnwindSafe};

use near_sdk::json_types::{U128, U64};
use near_sdk::{testing_env, PromiseResult, RuntimeFeesConfig};

use crate::test_support::*;
use crate::{AssetId, Decision, DenyReason, EpochSpend, Minder, OwnerChange};

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

/// Another key on the contract account, distinct from `agent_key()` (1) and
/// `other_key()` (2).
fn fresh_agent_key(byte: u8) -> near_sdk::PublicKey {
    near_sdk::PublicKey::from_parts(near_sdk::CurveType::ED25519, vec![byte; 32])
        .expect("a valid key")
}

/// The delay of the timelocked-reset tests, in ns.
const DELAY: u64 = 1_000;

/// Switches the timelock on (raising the delay is instant), as the owner.
fn timelocked(minder: &mut Minder) {
    as_owner();
    minder.set_timelock_delay(U64(DELAY));
}

fn reset_grant(key: near_sdk::PublicKey) -> OwnerChange {
    OwnerChange::GrantMandate {
        agent_key: key,
        expiry: U64(FAR_FUTURE),
        scope: None,
        reset_budget: Some(true),
    }
}

fn panic_text(call: impl FnOnce()) -> String {
    let payload = catch_unwind(AssertUnwindSafe(call)).expect_err("expected a panic");
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|text| text.to_string()))
        .unwrap_or_default()
}

#[test]
fn a_regrant_carries_the_budget_forward_and_the_key_epoch_moves_alone() {
    let mut minder = armed_with_budget(Some(BUDGET));
    spend(&mut minder, 50);
    spend(&mut minder, 20);
    assert_eq!(
        ask(&mut minder, 1),
        Decision::Deny(DenyReason::MandateBudgetExceeded)
    );

    as_owner();
    minder.revoke_mandate();
    minder.grant_mandate(other_key(), U64(FAR_FUTURE), None, None);
    as_agent(other_key());

    // The key epoch moved on, the budget period did not.
    assert_eq!(minder.get_mandate().expect("a mandate").epoch, 2);
    assert_eq!(minder.budget_epoch, 1);
    // The window is global and a grant does not reset it, so move past it:
    // what is left to examine is the budget alone.
    after_the_window(other_key());
    assert_eq!(minder.epoch_committed(&AssetId::Near), 70);
    assert_eq!(
        minder.epoch_spend.get(&AssetId::Near).copied(),
        Some(EpochSpend {
            epoch: 1,
            committed: 70
        })
    );
    // The new key cannot spend what the old one used up.
    assert_eq!(
        attempt(&mut minder, other_key(), 1),
        Decision::Deny(DenyReason::MandateBudgetExceeded)
    );
}

#[test]
fn partial_usage_carries_across_a_regrant_and_an_unused_budget_is_unchanged() {
    // Nothing spent: a re-grant leaves the whole budget, as ever.
    let mut minder = armed_with_budget(Some(BUDGET));
    as_owner();
    minder.revoke_mandate();
    minder.grant_mandate(other_key(), U64(FAR_FUTURE), None, None);
    as_agent(other_key());
    assert_eq!(attempt(&mut minder, other_key(), 50), Decision::Allow);

    // Some spent: exactly the rest remains after a re-grant.
    let mut minder = armed_with_budget(Some(BUDGET));
    spend(&mut minder, 30);
    as_owner();
    minder.revoke_mandate();
    minder.grant_mandate(other_key(), U64(FAR_FUTURE), None, None);
    as_agent(other_key());
    after_the_window(other_key());
    assert_eq!(attempt(&mut minder, other_key(), 40), Decision::Allow);
    settle(
        &mut minder,
        40,
        PromiseResult::Successful(Vec::new()),
        other_key(),
    );
    assert_eq!(
        attempt(&mut minder, other_key(), 1),
        Decision::Deny(DenyReason::MandateBudgetExceeded)
    );
}

#[test]
fn a_reset_with_nothing_spent_is_instant_and_changes_nothing() {
    let mut minder = armed_with_budget(Some(BUDGET));
    timelocked(&mut minder);
    // Nothing is spent, so asking for a fresh period hands nothing back.
    minder.grant_mandate(other_key(), U64(FAR_FUTURE), None, Some(true));
    assert_eq!(minder.budget_epoch, 1, "an unused period is not bumped");
    assert_eq!(minder.get_mandate().expect("a mandate").epoch, 2);
    as_agent(other_key());
    assert_eq!(attempt(&mut minder, other_key(), 50), Decision::Allow);
}

#[test]
fn a_reset_with_spend_is_a_timelocked_loosening_that_takes_the_delay() {
    let mut minder = armed_with_budget(Some(BUDGET));
    spend(&mut minder, 50);
    spend(&mut minder, 20);
    timelocked(&mut minder);
    as_owner();
    let key = fresh_agent_key(3);

    // Called directly it is refused, and the budget is still spent.
    let text = panic_text(|| minder.grant_mandate(key.clone(), U64(FAR_FUTURE), None, Some(true)));
    assert!(text.contains("Timelocked"), "{text}");
    assert_eq!(minder.budget_epoch, 1);

    // Proposed, it waits.
    let id = minder.propose_change(reset_grant(key.clone()));
    CLOCK.with(|clock| clock.set(clock.get() + DELAY - 1));
    as_owner();
    let text = panic_text(|| {
        let _ = minder.execute_change(id);
    });
    assert!(text.contains("Not ready"), "{text}");
    assert_eq!(minder.epoch_committed(&AssetId::Near), 70);

    // Once due it applies: a fresh period, usage zero, a new key epoch.
    CLOCK.with(|clock| clock.set(clock.get() + 1));
    as_owner();
    let _ = minder.execute_change(id);
    assert_eq!(minder.budget_epoch, 2);
    assert_eq!(minder.epoch_committed(&AssetId::Near), 0);
    assert_eq!(minder.get_mandate().expect("a mandate").epoch, 2);
    as_agent(key.clone());
    assert_eq!(attempt(&mut minder, key.clone(), 50), Decision::Allow);
    settle(
        &mut minder,
        50,
        PromiseResult::Successful(Vec::new()),
        key.clone(),
    );
    assert_eq!(attempt(&mut minder, key.clone(), 20), Decision::Allow);
    settle(
        &mut minder,
        20,
        PromiseResult::Successful(Vec::new()),
        key.clone(),
    );
    assert_eq!(
        attempt(&mut minder, key, 1),
        Decision::Deny(DenyReason::MandateBudgetExceeded)
    );
}

/// The bypass this closes: a compromised owner key, with the budget used up,
/// revokes and grants again (both instant) hoping for a fresh budget.
#[test]
fn a_compromised_owner_cannot_reset_a_used_budget_at_once() {
    let mut minder = armed_with_budget(Some(BUDGET));
    spend(&mut minder, 50);
    spend(&mut minder, 20);
    timelocked(&mut minder);
    as_owner();

    // Revoke and re-grant are instant; the budget does not come back.
    minder.revoke_mandate();
    let key = fresh_agent_key(4);
    minder.grant_mandate(key.clone(), U64(FAR_FUTURE), None, None);
    // Asking for the reset in the same call is refused until the delay has passed.
    let text =
        panic_text(|| minder.grant_mandate(fresh_agent_key(5), U64(FAR_FUTURE), None, Some(true)));
    assert!(text.contains("Timelocked"), "{text}");
    after_the_window(key.clone());
    assert_eq!(
        attempt(&mut minder, key, 1),
        Decision::Deny(DenyReason::MandateBudgetExceeded)
    );
}

#[test]
fn a_reset_after_a_revocation_is_still_a_loosening_when_spend_remains() {
    let mut minder = armed_with_budget(Some(BUDGET));
    spend(&mut minder, 10);
    timelocked(&mut minder);
    as_owner();
    minder.revoke_mandate();
    let text =
        panic_text(|| minder.grant_mandate(fresh_agent_key(5), U64(FAR_FUTURE), None, Some(true)));
    assert!(text.contains("Timelocked"), "{text}");
}

#[test]
fn a_reset_with_the_timelock_off_is_instant_and_hands_the_budget_back() {
    let mut minder = armed_with_budget(Some(BUDGET));
    spend(&mut minder, 50);
    spend(&mut minder, 20);
    as_owner();
    let key = fresh_agent_key(3);
    minder.grant_mandate(key.clone(), U64(FAR_FUTURE), None, Some(true));
    assert_eq!(minder.budget_epoch, 2);
    after_the_window(key.clone());
    assert_eq!(attempt(&mut minder, key, 50), Decision::Allow);
}

#[test]
fn removing_the_policy_does_not_let_a_reset_through_the_timelock() {
    // The usage flag is global, so dropping the budgeted asset's policy first
    // does not make a reset look free.
    let mut minder = armed_with_budget(Some(BUDGET));
    spend(&mut minder, 50);
    timelocked(&mut minder);
    as_owner();
    minder.remove_asset_policy(AssetId::Near);
    let text =
        panic_text(|| minder.grant_mandate(fresh_agent_key(3), U64(FAR_FUTURE), None, Some(true)));
    assert!(text.contains("Timelocked"), "{text}");
}

#[test]
fn spend_settled_after_a_regrant_stays_in_the_same_budget_period() {
    let mut minder = armed_with_budget(Some(BUDGET));
    assert_eq!(ask(&mut minder, 40), Decision::Allow); // in flight under epoch 1

    as_owner();
    minder.revoke_mandate();
    minder.grant_mandate(other_key(), U64(FAR_FUTURE), None, None);
    as_agent(other_key());

    // The in-flight 40 is still counted (reserved) against the budget ...
    assert_eq!(
        attempt(&mut minder, other_key(), 40),
        Decision::Deny(DenyReason::MandateBudgetExceeded)
    );
    // ... and when it settles it is booked to the period, never dropped.
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
fn lowering_the_budget_below_what_was_used_denies_until_the_budget_is_reset() {
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

/// A reset that lands while a request is in flight: the in-flight amount was
/// counted all along (reserved), and it is booked to the new period when it
/// settles, so the new period starts with exactly it.
#[test]
fn spend_in_flight_across_a_reset_is_booked_to_the_new_period() {
    let mut minder = armed_with_budget(Some(BUDGET));
    spend(&mut minder, 10);
    assert_eq!(ask(&mut minder, 40), Decision::Allow); // in flight
    as_owner();
    let key = fresh_agent_key(3);
    minder.grant_mandate(key.clone(), U64(FAR_FUTURE), None, Some(true));
    as_agent(key.clone());
    assert_eq!(minder.epoch_committed(&AssetId::Near), 0);
    // Still counted: 0 committed + 40 reserved leaves 30.
    assert_eq!(
        attempt(&mut minder, key.clone(), 31),
        Decision::Deny(DenyReason::MandateBudgetExceeded)
    );
    settle(&mut minder, 40, PromiseResult::Successful(Vec::new()), key);
    assert_eq!(minder.epoch_committed(&AssetId::Near), 40);
}
