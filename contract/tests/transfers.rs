//! End-to-end tests of the local transfer path in a sandbox: reservation,
//! the real asynchronous transfer, and settlement by callback. The NEP-141 side
//! runs against `test-contracts/mock-ft`. See DESIGN.md for the invariant map.

mod common;

use std::time::Duration;

use common::*;
use minder::{Decision, DenyReason};
use near_sdk::serde_json::json;
use near_workspaces::types::Gas;
use near_workspaces::AccountId;

const AMOUNT: u128 = 40;

fn ghost() -> AccountId {
    "ghost.test.near".parse().unwrap()
}

/// Everything that determines how much the agent can still spend in NEAR.
async fn native_budget(f: &Fixture) -> (u128, u128, Decision, Decision) {
    (
        f.live_spend(near()).await,
        f.reserved(near()).await,
        f.dry_run_as(f.agent.id(), near(), 40, &ghost()).await,
        f.dry_run_as(f.agent.id(), near(), 41, &ghost()).await,
    )
}

async fn balance_of(account: &near_workspaces::Account) -> u128 {
    account.view_account().await.unwrap().balance.as_yoctonear()
}

// ---- Settlement: successful transfers ----

#[tokio::test]
async fn native_transfer_pays_the_recipient_and_commits_the_spend_once() {
    let f = Fixture::armed().await;
    let before = balance_of(&f.recipient).await;

    let result = f.request_transfer(near(), AMOUNT, f.recipient.id()).await;
    result.into_result().unwrap();

    assert_eq!(balance_of(&f.recipient).await - before, AMOUNT);
    assert_eq!(f.reserved(near()).await, 0);
    assert_eq!(f.live_spend(near()).await, AMOUNT, "committed exactly once");
}

#[tokio::test]
async fn token_transfer_calls_ft_transfer_and_commits_the_spend_once() {
    let f = Fixture::armed().await;
    let (token, asset) = f.deploy_token().await;
    f.set_policy(asset.clone(), PER_TX_CAP, WINDOW_LIMIT).await;

    // The mock insists on exactly 1 yoctoNEAR, and has no ft_transfer_call.
    f.request_transfer(asset.clone(), AMOUNT, f.recipient.id())
        .await
        .into_result()
        .unwrap();

    let transfers: Vec<(AccountId, String)> =
        token.view("get_transfers").await.unwrap().json().unwrap();
    assert_eq!(
        transfers,
        vec![(f.recipient.id().clone(), AMOUNT.to_string())]
    );
    assert_eq!(f.reserved(asset.clone()).await, 0);
    assert_eq!(f.live_spend(asset).await, AMOUNT, "committed exactly once");
}

// ---- Settlement: failed transfers revert cleanly ----

#[tokio::test]
async fn failed_native_transfer_restores_the_budget() {
    let f = Fixture::armed().await;
    f.owner_ok(
        "set_recipient",
        json!({ "recipient": ghost(), "allowed": true }),
    )
    .await;
    f.record_spend(near(), 60).await;
    let before = native_budget(&f).await;
    assert_eq!(
        before,
        (
            60,
            0,
            Decision::Allow,
            deny(DenyReason::WindowLimitExceeded)
        )
    );

    // The recipient account does not exist, so the transfer fails and refunds.
    let result = f.request_transfer(near(), 40, &ghost()).await;
    assert!(
        result.is_success(),
        "the request itself succeeds: {result:?}"
    );

    assert_eq!(native_budget(&f).await, before);
}

#[tokio::test]
async fn failed_token_transfer_restores_the_budget() {
    let f = Fixture::armed().await;
    let (token, asset) = f.deploy_token().await;
    f.set_policy(asset.clone(), PER_TX_CAP, WINDOW_LIMIT).await;
    f.record_spend(asset.clone(), 60).await;
    token
        .call("set_fail_transfers")
        .args_json(json!({ "fail": true }))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();

    let result = f
        .request_transfer(asset.clone(), 40, f.recipient.id())
        .await;
    assert!(
        result.is_success(),
        "the request itself succeeds: {result:?}"
    );

    assert_eq!(f.reserved(asset.clone()).await, 0);
    assert_eq!(
        f.live_spend(asset.clone()).await,
        60,
        "window ring unchanged"
    );
    assert_eq!(f.dry_run(asset.clone(), 40).await, Decision::Allow);
    assert_eq!(
        f.dry_run(asset, 41).await,
        deny(DenyReason::WindowLimitExceeded)
    );
    let transfers: Vec<(AccountId, String)> =
        token.view("get_transfers").await.unwrap().json().unwrap();
    assert!(transfers.is_empty());
}

// ---- No stranded reservation ----

#[tokio::test]
async fn denied_request_reverts_and_reserves_nothing() {
    let f = Fixture::armed().await;
    let before = balance_of(&f.recipient).await;

    assert_reverts(
        f.request_transfer(near(), PER_TX_CAP + 1, f.recipient.id())
            .await,
        "Denied: PerTxCapExceeded",
    );
    assert_reverts(
        f.request_transfer(near(), 1, f.outsider.id()).await,
        "Denied: RecipientNotAllowed",
    );
    assert_reverts(
        f.call(
            &f.outsider,
            "request_local_transfer",
            json!({ "asset": near(), "amount": "1", "recipient": f.recipient.id() }),
        )
        .await,
        "Denied: NotAuthorized",
    );

    assert_eq!(f.reserved(near()).await, 0);
    assert_eq!(f.live_spend(near()).await, 0);
    assert_eq!(balance_of(&f.recipient).await, before);
}

#[tokio::test]
async fn request_without_gas_for_the_callback_reverts_and_reserves_nothing() {
    let f = Fixture::armed().await;
    let (token, asset) = f.deploy_token().await;
    f.set_policy(asset.clone(), PER_TX_CAP, WINDOW_LIMIT).await;

    // The transfer and callback gas are attached up front; too little prepaid
    // gas makes promise creation fail, which reverts the reservation with it.
    let result = f
        .agent
        .call(f.contract.id(), "request_local_transfer")
        .args_json(
            json!({ "asset": asset, "amount": AMOUNT.to_string(), "recipient": f.recipient.id() }),
        )
        .gas(Gas::from_tgas(40))
        .transact()
        .await
        .unwrap();
    assert!(result.is_failure());

    assert_eq!(f.reserved(asset).await, 0);
    let transfers: Vec<(AccountId, String)> =
        token.view("get_transfers").await.unwrap().json().unwrap();
    assert!(transfers.is_empty());
}

// ---- In flight ----

#[tokio::test]
async fn reservation_is_visible_and_binding_while_the_transfer_is_in_flight() {
    let f = Fixture::armed().await;
    let (token, asset) = f.deploy_token().await;
    // Limit 60 with a 50 transfer in flight leaves room for only 10 more.
    f.set_policy(asset.clone(), PER_TX_CAP, 60).await;
    token
        .call("set_slow_hops")
        .args_json(json!({ "hops": 6 }))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();

    let pending = f
        .agent
        .call(f.contract.id(), "request_local_transfer")
        .args_json(json!({ "asset": asset, "amount": "50", "recipient": f.recipient.id() }))
        .gas(Gas::from_tgas(300))
        .transact_async()
        .await
        .unwrap();

    // Poll until the reservation shows; it is released only by the callback,
    // so seeing it means the transfer has not settled.
    let mut seen = false;
    for _ in 0..600 {
        if f.reserved(asset.clone()).await == 50 {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen, "never observed the in-flight reservation");
    assert_eq!(
        f.live_spend(asset.clone()).await,
        0,
        "not committed before settlement"
    );
    assert_eq!(f.dry_run(asset.clone(), 10).await, Decision::Allow);
    assert_eq!(
        f.dry_run(asset.clone(), 11).await,
        deny(DenyReason::WindowLimitExceeded)
    );

    pending.await.unwrap().into_result().unwrap();

    assert_eq!(f.reserved(asset.clone()).await, 0);
    assert_eq!(f.live_spend(asset.clone()).await, 50);
    assert_eq!(f.dry_run(asset.clone(), 10).await, Decision::Allow);
    assert_eq!(
        f.dry_run(asset, 11).await,
        deny(DenyReason::WindowLimitExceeded)
    );
}

// ---- Callback access ----

#[tokio::test]
async fn only_the_contract_itself_can_call_the_settlement_callback() {
    let f = Fixture::armed().await;
    for who in [&f.owner, &f.agent, &f.outsider] {
        assert_reverts(
            f.call(
                who,
                "on_transfer_complete",
                json!({ "asset": near(), "amount": "1" }),
            )
            .await,
            "Method on_transfer_complete is private",
        );
    }
    assert_eq!(f.live_spend(near()).await, 0);
}
