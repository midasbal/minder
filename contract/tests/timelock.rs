//! The asymmetric timelock in a sandbox, against the real owner account: what a
//! compromised owner key is forced through, and that a legitimate emergency
//! action stays instant.

mod common;

use common::*;
use near_sdk::serde_json::{json, Value};
use near_workspaces::types::NearToken;

/// The delay of these tests: long enough to observe, short enough to wait out.
const DELAY: u64 = 20 * SECOND;

/// The armed fixture (the timelock is off while it is armed, since arming adds
/// assets and recipients), with the recipient account as the NEAR treasury and
/// the timelock then switched on.
async fn timelocked() -> Fixture {
    let f = Fixture::armed().await;
    f.owner_ok(
        "set_treasury",
        json!({ "near": f.recipient.id(), "evm": null }),
    )
    .await;
    f.owner_ok("set_timelock_delay", json!({ "delay": DELAY.to_string() }))
        .await;
    f
}

async fn pending(f: &Fixture) -> Vec<Value> {
    f.view("get_pending_changes", json!({})).await
}

async fn balance(account: &near_workspaces::Account) -> u128 {
    account.view_account().await.unwrap().balance.as_yoctonear()
}

#[tokio::test]
async fn a_compromised_owner_cannot_loosen_or_redirect_at_once_but_can_still_freeze() {
    let f = timelocked().await;
    let attacker = f.outsider.id();
    let before: Value = f.view("get_config", json!({})).await;

    // Each of these needs the owner key, which the attacker has, and each is
    // refused as timelocked: a raised cap, a new recipient, a new treasury, a
    // shorter delay, and a withdrawal to the attacker's own account.
    let attempts = [
        (
            "set_asset_policy",
            json!({ "asset": near(), "per_tx_cap": "1000000", "window_limit": "1000000" }),
        ),
        (
            "set_recipient",
            json!({ "recipient": attacker, "allowed": true }),
        ),
        ("set_treasury", json!({ "near": attacker, "evm": null })),
        ("set_timelock_delay", json!({ "delay": "0" })),
        (
            "owner_withdraw",
            json!({ "asset": near(), "amount": "1000000000000000000000000", "to": attacker }),
        ),
    ];
    let balance_before = balance(&f.outsider).await;
    for (method, args) in attempts {
        assert_reverts(f.call(&f.owner, method, args).await, "Timelocked");
    }
    assert_eq!(
        balance(&f.outsider).await,
        balance_before,
        "nothing was sent"
    );
    assert_eq!(f.view::<Value>("get_config", json!({})).await, before);
    assert_eq!(
        f.dry_run_as(&f.agent_key, near(), 1, attacker).await,
        deny(minder::DenyReason::RecipientNotAllowed)
    );

    // A legitimate emergency stays instant, and so does a tightening.
    f.owner_ok("freeze", json!({})).await;
    assert_eq!(f.dry_run(near(), 1).await, deny(minder::DenyReason::Frozen));
    f.owner_ok(
        "set_asset_policy",
        json!({ "asset": near(), "per_tx_cap": "10", "window_limit": "20" }),
    )
    .await;
    f.owner_ok("revoke_mandate", json!({})).await;
}

#[tokio::test]
async fn a_loosening_change_takes_effect_only_after_propose_wait_and_execute() {
    let f = timelocked().await;
    let change = json!({ "SetRecipient": { "recipient": f.outsider.id(), "allowed": true } });

    let id: String = {
        let result = f
            .call(&f.owner, "propose_change", json!({ "change": change }))
            .await;
        result.into_result().unwrap().json().unwrap()
    };
    let queue = pending(&f).await;
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0]["id"], id);
    let ready_at: u64 = queue[0]["ready_at"].as_str().unwrap().parse().unwrap();
    let proposed_at: u64 = queue[0]["proposed_at"].as_str().unwrap().parse().unwrap();
    assert_eq!(ready_at, proposed_at + DELAY);

    // Executing early is refused, and nothing has changed.
    assert_reverts(
        f.call(&f.owner, "execute_change", json!({ "id": id }))
            .await,
        "Not ready",
    );
    let allowed: bool = f
        .view(
            "is_recipient_allowed",
            json!({ "recipient": f.outsider.id() }),
        )
        .await;
    assert!(!allowed);

    // Once the delay has passed it applies, and is no longer pending.
    f.advance_to(ready_at + SECOND).await;
    f.owner_ok("execute_change", json!({ "id": id })).await;
    let allowed: bool = f
        .view(
            "is_recipient_allowed",
            json!({ "recipient": f.outsider.id() }),
        )
        .await;
    assert!(allowed);
    assert!(pending(&f).await.is_empty());
    // Only the owner can do any of it.
    assert_reverts(
        f.call(&f.outsider, "propose_change", json!({ "change": change }))
            .await,
        "Only the owner",
    );
}

#[tokio::test]
async fn a_cancelled_change_never_takes_effect() {
    let f = timelocked().await;
    let change = json!({ "SetRecipient": { "recipient": f.outsider.id(), "allowed": true } });
    let id: String = f
        .call(&f.owner, "propose_change", json!({ "change": change }))
        .await
        .into_result()
        .unwrap()
        .json()
        .unwrap();
    f.owner_ok("cancel_change", json!({ "id": id })).await;
    assert!(pending(&f).await.is_empty());
    f.advance_to(f.now().await + DELAY + SECOND).await;
    assert_reverts(
        f.call(&f.owner, "execute_change", json!({ "id": id }))
            .await,
        "No such pending change",
    );
}

#[tokio::test]
async fn owner_recovery_is_instant_to_the_treasury_and_delayed_anywhere_else() {
    let f = timelocked().await;
    let amount = NearToken::from_near(1).as_yoctonear();

    // To the treasury: at once.
    let treasury_before = balance(&f.recipient).await;
    f.owner_ok(
        "owner_withdraw",
        json!({ "asset": near(), "amount": amount.to_string(), "to": f.recipient.id() }),
    )
    .await;
    assert_eq!(balance(&f.recipient).await - treasury_before, amount);

    // To anywhere else: refused, and only possible through the delay.
    let elsewhere = f.outsider.id();
    let args = json!({ "asset": near(), "amount": amount.to_string(), "to": elsewhere });
    assert_reverts(
        f.call(&f.owner, "owner_withdraw", args.clone()).await,
        "Timelocked",
    );
    let id: String = f
        .call(
            &f.owner,
            "propose_change",
            json!({ "change": { "OwnerWithdraw": args } }),
        )
        .await
        .into_result()
        .unwrap()
        .json()
        .unwrap();
    let before = balance(&f.outsider).await;
    let ready_at: u64 = pending(&f).await[0]["ready_at"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    f.advance_to(ready_at + SECOND).await;
    f.owner_ok("execute_change", json!({ "id": id })).await;
    assert_eq!(
        balance(&f.outsider).await - before,
        amount,
        "paid on execution"
    );
}

#[tokio::test]
async fn the_timelock_can_be_enabled_after_arming_and_never_loosened_instantly() {
    let f = Fixture::armed().await; // the timelock is off: loosening is instant
    f.owner_ok(
        "set_asset_policy",
        json!({ "asset": near(), "per_tx_cap": "500", "window_limit": "900" }),
    )
    .await;
    f.owner_ok("set_timelock_delay", json!({ "delay": DELAY.to_string() }))
        .await;
    assert_reverts(
        f.call(
            &f.owner,
            "set_asset_policy",
            json!({ "asset": near(), "per_tx_cap": "501", "window_limit": "900" }),
        )
        .await,
        "Timelocked",
    );
    // Raising the delay stays instant; lowering it is held.
    f.owner_ok(
        "set_timelock_delay",
        json!({ "delay": (2 * DELAY).to_string() }),
    )
    .await;
    assert_reverts(
        f.call(&f.owner, "set_timelock_delay", json!({ "delay": "1" }))
            .await,
        "Timelocked",
    );
}

#[tokio::test]
async fn init_takes_the_delay_and_the_treasuries() {
    let f = Fixture::deploy().await;
    let mut args = f.init_args();
    args["timelock_delay"] = json!(DELAY.to_string());
    args["near_treasury"] = json!(f.recipient.id());
    args["evm_treasury"] = json!("0x00000000000000000000000000000000000000AA");
    f.contract
        .call("init")
        .args_json(args)
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let config: Value = f.view("get_config", json!({})).await;
    assert_eq!(config["timelock_delay"], DELAY.to_string());
    assert_eq!(config["near_treasury"], f.recipient.id().to_string());
    assert_eq!(
        config["evm_treasury"], "0x00000000000000000000000000000000000000aa",
        "stored lowercase"
    );
}
