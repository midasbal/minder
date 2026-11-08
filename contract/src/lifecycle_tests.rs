//! Unit tests for the reservation lifecycle under the mocked NEAR environment:
//! what `request_local_transfer` reserves and dispatches, and how
//! `on_transfer_complete` settles every possible promise result.

use std::panic::{catch_unwind, AssertUnwindSafe};

use near_sdk::json_types::{U128, U64};
use near_sdk::mock::MockAction;
use near_sdk::serde_json::{json, Value};
use near_sdk::test_utils::{get_created_receipts, VMContextBuilder};
use near_sdk::{testing_env, AccountId, Gas, NearToken, PromiseResult, RuntimeFeesConfig};

use crate::{AssetId, Decision, DenyReason, Minder};

const PER_TX_CAP: u128 = 50;
const WINDOW_LIMIT: u128 = 100;

fn account(name: &str) -> AccountId {
    name.parse().unwrap()
}

fn token() -> AssetId {
    AssetId::Nep141(account("usdc.near"))
}

fn context(predecessor: &str) -> VMContextBuilder {
    let mut builder = VMContextBuilder::new();
    builder
        .current_account_id(account("minder.near"))
        .predecessor_account_id(account(predecessor))
        .block_timestamp(1_000_000)
        .prepaid_gas(Gas::from_tgas(300));
    builder
}

/// Initialized, with policies for NEAR and a token, an allowed recipient and a
/// mandate for `agent.near`. The environment is left as the agent.
fn armed_as_agent() -> Minder {
    testing_env!(context("owner.near").build());
    let mut minder = Minder::init(account("owner.near"), U64(400), 4);
    for asset in [AssetId::Near, token()] {
        minder.set_asset_policy(asset, U128(PER_TX_CAP), U128(WINDOW_LIMIT));
    }
    minder.set_recipient(account("bob.near"), true);
    minder.grant_mandate(account("agent.near"), U64(u64::MAX));
    testing_env!(context("agent.near").build());
    minder
}

/// Runs the settlement callback as the contract itself, with the given
/// result for the transfer promise, then returns the environment to the agent.
fn settle(minder: &mut Minder, asset: AssetId, amount: u128, result: PromiseResult) {
    testing_env!(
        context("minder.near").build(),
        near_sdk::test_vm_config(),
        RuntimeFeesConfig::test(),
        Default::default(),
        vec![result]
    );
    minder.on_transfer_complete(asset, U128(amount));
    testing_env!(context("agent.near").build());
}

fn request(minder: &mut Minder, asset: AssetId, amount: u128) {
    let _ = minder.request_local_transfer(asset, U128(amount), account("bob.near"));
}

fn dry_run(minder: &Minder, asset: AssetId, amount: u128) -> Decision {
    minder.dry_run(
        account("agent.near"),
        asset,
        U128(amount),
        account("bob.near"),
    )
}

fn reserved(minder: &Minder, asset: AssetId) -> u128 {
    minder.get_reserved(asset).0
}

fn committed(minder: &Minder, asset: AssetId) -> u128 {
    minder.get_live_spend(asset).0
}

// ---- Reservation ----

#[test]
fn reservation_is_visible_before_the_callback_runs() {
    let mut minder = armed_as_agent();
    request(&mut minder, AssetId::Near, 40);

    // In flight: reserved, not yet committed.
    assert_eq!(reserved(&minder, AssetId::Near), 40);
    assert_eq!(committed(&minder, AssetId::Near), 0);
    // The reservation counts against the budget: 40 + 60 = limit, 40 + 61 is over.
    assert_eq!(dry_run(&minder, AssetId::Near, 50), Decision::Allow);
    request(&mut minder, AssetId::Near, 50);
    assert_eq!(
        dry_run(&minder, AssetId::Near, 11),
        Decision::Deny(DenyReason::WindowLimitExceeded)
    );
    assert_eq!(dry_run(&minder, AssetId::Near, 10), Decision::Allow);
}

#[test]
fn a_call_made_while_another_is_in_flight_cannot_exceed_the_limit() {
    let mut minder = armed_as_agent();
    request(&mut minder, AssetId::Near, 50);
    request(&mut minder, AssetId::Near, 50);

    // Limit is fully reserved; no callback has run, so nothing is committed.
    assert_eq!(committed(&minder, AssetId::Near), 0);
    let attempt = catch_unwind(AssertUnwindSafe(|| {
        request(&mut minder, AssetId::Near, 1);
    }));
    assert!(panic_message(attempt).contains("WindowLimitExceeded"));
    assert_eq!(reserved(&minder, AssetId::Near), 100);
}

#[test]
fn reservations_are_tracked_per_asset() {
    let mut minder = armed_as_agent();
    request(&mut minder, AssetId::Near, 50);
    assert_eq!(reserved(&minder, token()), 0);
    assert_eq!(dry_run(&minder, token(), 50), Decision::Allow);
}

#[test]
fn a_denied_request_reserves_nothing() {
    let mut minder = armed_as_agent();
    let attempt = catch_unwind(AssertUnwindSafe(|| {
        request(&mut minder, AssetId::Near, PER_TX_CAP + 1);
    }));
    assert!(panic_message(attempt).contains("Denied: PerTxCapExceeded"));
    assert_eq!(reserved(&minder, AssetId::Near), 0);
    assert!(get_created_receipts().is_empty());
}

// ---- Dispatch ----

#[test]
fn native_request_creates_a_transfer_then_a_private_callback() {
    let mut minder = armed_as_agent();
    request(&mut minder, AssetId::Near, 40);

    let receipts = get_created_receipts();
    let transfer = receipts
        .iter()
        .find(|receipt| receipt.receiver_id == account("bob.near"))
        .expect("transfer receipt");
    assert!(transfer.actions.iter().any(|action| matches!(
        action,
        MockAction::Transfer { deposit, .. } if *deposit == NearToken::from_yoctonear(40)
    )));

    let callback = function_call(&receipts, "minder.near", "on_transfer_complete");
    assert_eq!(callback.args, json!({ "asset": "Near", "amount": "40" }));
    assert!(callback.gas >= Gas::from_tgas(15));
}

#[test]
fn token_request_calls_plain_ft_transfer_with_one_yocto() {
    let mut minder = armed_as_agent();
    request(&mut minder, token(), 40);

    let receipts = get_created_receipts();
    let transfer = function_call(&receipts, "usdc.near", "ft_transfer");
    assert_eq!(
        transfer.args,
        json!({ "receiver_id": "bob.near", "amount": "40", "memo": null })
    );
    assert_eq!(transfer.deposit, NearToken::from_yoctonear(1));
    assert!(
        receipts.iter().all(|receipt| {
            receipt.actions.iter().all(|action| !matches!(
            action,
            MockAction::FunctionCallWeight { method_name, .. } if method_name == b"ft_transfer_call"
        ))
        }),
        "must never call ft_transfer_call"
    );

    let callback = function_call(&receipts, "minder.near", "on_transfer_complete");
    assert_eq!(
        callback.args,
        json!({ "asset": { "Nep141": "usdc.near" }, "amount": "40" })
    );
}

// ---- Settlement ----

#[test]
fn successful_transfer_moves_the_reservation_into_the_window_once() {
    let mut minder = armed_as_agent();
    request(&mut minder, AssetId::Near, 40);
    settle(
        &mut minder,
        AssetId::Near,
        40,
        PromiseResult::Successful(vec![]),
    );

    assert_eq!(reserved(&minder, AssetId::Near), 0);
    assert_eq!(committed(&minder, AssetId::Near), 40);
    // Budget used is unchanged by settlement: still 40 of 100.
    assert_eq!(dry_run(&minder, AssetId::Near, 50), Decision::Allow);
    request(&mut minder, AssetId::Near, 50);
    assert_eq!(
        dry_run(&minder, AssetId::Near, 11),
        Decision::Deny(DenyReason::WindowLimitExceeded)
    );
}

#[test]
fn failed_transfer_releases_the_reservation_and_commits_nothing() {
    let mut minder = armed_as_agent();
    let before = dry_run(&minder, AssetId::Near, 50);
    request(&mut minder, AssetId::Near, 50);
    settle(&mut minder, AssetId::Near, 50, PromiseResult::Failed);

    assert_eq!(reserved(&minder, AssetId::Near), 0);
    assert_eq!(committed(&minder, AssetId::Near), 0);
    assert_eq!(dry_run(&minder, AssetId::Near, 50), before);
}

#[test]
fn an_unexpected_success_payload_still_counts_as_spent() {
    // Over-counting is safe; forgetting a transfer that did happen is not.
    let mut minder = armed_as_agent();
    request(&mut minder, token(), 30);
    settle(
        &mut minder,
        token(),
        30,
        PromiseResult::Successful(vec![1, 2, 3]),
    );

    assert_eq!(reserved(&minder, token()), 0);
    assert_eq!(committed(&minder, token()), 30);
}

#[test]
fn settlement_never_panics_even_if_it_releases_more_than_is_reserved() {
    let mut minder = armed_as_agent();
    request(&mut minder, AssetId::Near, 10);
    settle(&mut minder, AssetId::Near, 25, PromiseResult::Failed);
    assert_eq!(reserved(&minder, AssetId::Near), 0);

    // And with nothing reserved at all.
    settle(&mut minder, AssetId::Near, 25, PromiseResult::Failed);
    assert_eq!(reserved(&minder, AssetId::Near), 0);
}

#[test]
fn settlement_is_independent_of_policy_changes_made_in_flight() {
    let mut minder = armed_as_agent();
    request(&mut minder, AssetId::Near, 40);

    testing_env!(context("owner.near").build());
    minder.freeze();
    minder.remove_asset_policy(AssetId::Near);

    settle(
        &mut minder,
        AssetId::Near,
        40,
        PromiseResult::Successful(vec![]),
    );
    assert_eq!(reserved(&minder, AssetId::Near), 0);
    assert_eq!(committed(&minder, AssetId::Near), 40);
}

// ---- Helpers ----

struct FunctionCall {
    args: Value,
    deposit: NearToken,
    gas: Gas,
}

fn function_call(
    receipts: &[near_sdk::mock::Receipt],
    receiver: &str,
    method: &str,
) -> FunctionCall {
    receipts
        .iter()
        .filter(|receipt| receipt.receiver_id == account(receiver))
        .flat_map(|receipt| &receipt.actions)
        .find_map(|action| match action {
            MockAction::FunctionCallWeight {
                method_name,
                args,
                attached_deposit,
                prepaid_gas,
                ..
            } if method_name == method.as_bytes() => Some(FunctionCall {
                args: near_sdk::serde_json::from_slice(args).unwrap(),
                deposit: *attached_deposit,
                gas: *prepaid_gas,
            }),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {method} call to {receiver}"))
}

fn panic_message(result: std::thread::Result<()>) -> String {
    let payload = result.expect_err("expected a panic");
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}
