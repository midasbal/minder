//! Fund custody in a sandbox: owner withdrawal as recovery, and the two ways
//! of funding the contract. See DESIGN.md for the invariant map.

mod common;

use std::time::Duration;

use common::*;
use minder::{Decision, DenyReason};
use near_sdk::serde_json::json;
use near_workspaces::types::{Gas, NearToken};
use near_workspaces::{Account, AccountId, Contract};

/// Larger than both the per-transaction cap and the window limit.
const WITHDRAWAL: u128 = 500;
const _: () = assert!(WITHDRAWAL > PER_TX_CAP && WITHDRAWAL > WINDOW_LIMIT);

async fn balance_of(account: &Account) -> u128 {
    account.view_account().await.unwrap().balance.as_yoctonear()
}

async fn contract_balance(f: &Fixture) -> u128 {
    f.contract
        .view_account()
        .await
        .unwrap()
        .balance
        .as_yoctonear()
}

async fn withdraw(
    f: &Fixture,
    who: &Account,
    asset: near_sdk::serde_json::Value,
    to: &AccountId,
) -> near_workspaces::result::ExecutionFinalResult {
    who.call(f.contract.id(), "owner_withdraw")
        .args_json(json!({ "asset": asset, "amount": WITHDRAWAL.to_string(), "to": to }))
        .gas(Gas::from_tgas(300))
        .transact()
        .await
        .unwrap()
}

async fn transfers_recorded(token: &Contract) -> Vec<(AccountId, String)> {
    token.view("get_transfers").await.unwrap().json().unwrap()
}

// ---- owner_withdraw ----

#[tokio::test]
async fn only_the_owner_can_withdraw() {
    let f = Fixture::armed().await;
    // Watch the payee, not the caller: callers pay gas for their own attempt.
    let before = balance_of(&f.recipient).await;
    for who in [&f.outsider] {
        assert_reverts(
            withdraw(&f, who, near(), f.recipient.id()).await,
            "Only the owner can call this method",
        );
    }
    assert_eq!(balance_of(&f.recipient).await, before);
}

#[tokio::test]
async fn owner_recovers_funds_while_frozen() {
    let f = Fixture::armed().await;
    f.owner_ok("freeze", json!({})).await;
    f.owner_ok("revoke_mandate", json!({})).await;
    // The agent is stopped...
    assert_eq!(f.dry_run(near(), 1).await, deny(DenyReason::Frozen));

    // ...the owner is not.
    let before = balance_of(&f.outsider).await;
    withdraw(&f, &f.owner, near(), f.outsider.id())
        .await
        .into_result()
        .unwrap();
    assert_eq!(balance_of(&f.outsider).await - before, WITHDRAWAL);
}

#[tokio::test]
async fn native_withdraw_pays_the_recipient_without_regard_to_policy() {
    // `to` is not allowlisted and the amount exceeds every cap.
    let f = Fixture::armed().await;
    let before = balance_of(&f.outsider).await;

    withdraw(&f, &f.owner, near(), f.outsider.id())
        .await
        .into_result()
        .unwrap();

    assert_eq!(balance_of(&f.outsider).await - before, WITHDRAWAL);
}

#[tokio::test]
async fn token_withdraw_pays_the_recipient_without_regard_to_policy() {
    // The token has no policy at all, and `to` is not allowlisted.
    let f = Fixture::armed().await;
    let (token, asset) = f.deploy_token().await;

    withdraw(&f, &f.owner, asset, f.outsider.id())
        .await
        .into_result()
        .unwrap();

    assert_eq!(
        transfers_recorded(&token).await,
        vec![(f.outsider.id().clone(), WITHDRAWAL.to_string())]
    );
}

#[tokio::test]
async fn failed_withdrawal_fails_the_promise_and_changes_no_accounting() {
    let f = Fixture::armed().await;
    let (token, asset) = f.deploy_token().await;
    f.set_policy(asset.clone(), PER_TX_CAP, WINDOW_LIMIT).await;
    f.record_spend(asset.clone(), 20).await;
    token
        .call("set_fail_transfers")
        .args_json(json!({ "fail": true }))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();

    let result = withdraw(&f, &f.owner, asset.clone(), f.outsider.id()).await;
    assert!(result.is_failure());

    assert_eq!(f.reserved(asset.clone()).await, 0);
    assert_eq!(f.live_spend(asset).await, 20);
}

#[tokio::test]
async fn owner_withdraw_does_not_touch_reservations_or_the_window() {
    let f = Fixture::armed().await;
    let (token, asset) = f.deploy_token().await;
    f.set_policy(asset.clone(), PER_TX_CAP, WINDOW_LIMIT).await;
    f.record_spend(asset.clone(), 20).await;
    token
        .call("set_slow_hops")
        .args_json(json!({ "hops": 6 }))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();

    // Put a transfer of 50 in flight: reserved 50, committed 20 until it settles.
    let pending = f
        .agent
        .call(f.contract.id(), "request_local_transfer")
        .args_json(json!({ "asset": asset, "amount": "50", "recipient": f.recipient.id() }))
        .gas(Gas::from_tgas(AGENT_TGAS))
        .transact_async()
        .await
        .unwrap();
    let mut seen = false;
    for _ in 0..600 {
        if f.reserved(asset.clone()).await == 50 {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen, "never observed the in-flight reservation");

    // Withdraw native funds while it is in flight.
    withdraw(&f, &f.owner, near(), f.outsider.id())
        .await
        .into_result()
        .unwrap();

    // Either the transfer is still in flight (50, 20) or it has settled by now
    // (0, 70). Any other pair would mean the withdrawal touched accounting.
    let state = (
        f.reserved(asset.clone()).await,
        f.live_spend(asset.clone()).await,
    );
    assert!(
        state == (50, 20) || state == (0, 70),
        "withdrawal disturbed accounting: reserved/committed = {state:?}"
    );
    // The withdrawal also left the native asset's accounting alone.
    assert_eq!(f.reserved(near()).await, 0);
    assert_eq!(f.live_spend(near()).await, 0);

    pending.await.unwrap().into_result().unwrap();
    assert_eq!(f.reserved(asset.clone()).await, 0);
    assert_eq!(f.live_spend(asset).await, 70);
}

// ---- Funding ----

#[tokio::test]
async fn native_deposit_increases_the_contract_balance() {
    let f = Fixture::armed().await;
    let before = contract_balance(&f).await;
    let deposit = NearToken::from_near(1);

    // Open to anyone, not just the owner.
    f.outsider
        .call(f.contract.id(), "deposit")
        .deposit(deposit)
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();

    assert!(contract_balance(&f).await >= before + deposit.as_yoctonear());
}

#[tokio::test]
async fn deposit_without_an_attached_amount_reverts() {
    let f = Fixture::armed().await;
    assert_reverts(
        f.call(&f.outsider, "deposit", json!({})).await,
        "Attach a deposit",
    );
}

#[tokio::test]
async fn ft_on_transfer_accepts_a_token_that_has_a_policy() {
    let f = Fixture::armed().await;
    let (token, asset) = f.deploy_token().await;
    f.set_policy(asset, PER_TX_CAP, WINDOW_LIMIT).await;

    send_tokens_to_minder(&f, &token, 70).await;

    // The mock records what the receiver kept: all of it, refund 0.
    assert_eq!(
        transfers_recorded(&token).await,
        vec![(f.contract.id().clone(), "70".to_string())]
    );
}

#[tokio::test]
async fn ft_on_transfer_refunds_a_token_without_a_policy_in_full() {
    let f = Fixture::armed().await;
    let (token, _) = f.deploy_token().await;

    send_tokens_to_minder(&f, &token, 70).await;

    assert!(transfers_recorded(&token).await.is_empty());
}

#[tokio::test]
async fn ft_on_transfer_leaves_accounting_alone() {
    let f = Fixture::armed().await;
    let (token, asset) = f.deploy_token().await;
    f.set_policy(asset.clone(), PER_TX_CAP, WINDOW_LIMIT).await;

    send_tokens_to_minder(&f, &token, 70).await;

    assert_eq!(f.reserved(asset.clone()).await, 0);
    assert_eq!(f.live_spend(asset.clone()).await, 0);
    assert_eq!(f.dry_run(asset, PER_TX_CAP).await, Decision::Allow);
}

/// A third party sends tokens with `ft_transfer_call`, the standard way to
/// fund a contract with a token.
async fn send_tokens_to_minder(f: &Fixture, token: &Contract, amount: u128) {
    f.outsider
        .call(token.id(), "ft_transfer_call")
        .args_json(
            json!({ "receiver_id": f.contract.id(), "amount": amount.to_string(), "msg": "" }),
        )
        .deposit(NearToken::from_yoctonear(1))
        .gas(Gas::from_tgas(300))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
}
