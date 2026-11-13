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

// ---- The guardian ----

#[tokio::test]
async fn init_takes_a_guardian() {
    let f = Fixture::deploy().await;
    let mut args = f.init_args();
    args["guardian"] = json!(f.outsider.id());
    f.contract
        .call("init")
        .args_json(args)
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let config: Value = f.view("get_config", json!({})).await;
    assert_eq!(config["guardian"], json!(f.outsider.id()));
}

/// The guardian is the account `outsider`. It freezes, revokes and cancels a
/// pending change through the real runtime, and every other owner method, called
/// with its key, reverts without moving funds or changing the configuration.
#[tokio::test]
async fn a_compromised_guardian_can_only_stop_never_move_or_widen() {
    let f = timelocked().await;
    f.owner_ok("set_guardian", json!({ "guardian": f.outsider.id() }))
        .await;
    let guardian = &f.outsider;

    // A change the owner queued, for the guardian to cancel.
    let queued = json!({ "SetAssetPolicy": { "asset": near(), "per_tx_cap": "1000000", "window_limit": "1000000", "epoch_budget": null } });
    let id: String = f
        .call(&f.owner, "propose_change", json!({ "change": queued }))
        .await
        .into_result()
        .unwrap()
        .json()
        .unwrap();

    // Everything but the three stop-powers is refused to the guardian.
    let before: Value = f.view("get_config", json!({})).await;
    let attacker = guardian.id();
    let balance_before = balance(guardian).await;
    let contract_before = balance(f.contract.as_account()).await;
    let attempts = [
        (
            "set_asset_policy",
            json!({ "asset": near(), "per_tx_cap": "1", "window_limit": "1" }),
        ),
        (
            "set_recipient",
            json!({ "recipient": attacker, "allowed": true }),
        ),
        (
            "grant_mandate",
            json!({ "agent_key": fresh_key(), "expiry": "99999999999999999" }),
        ),
        ("unfreeze", json!({})),
        (
            "owner_withdraw",
            json!({ "asset": near(), "amount": "1000000000000000000000000", "to": attacker }),
        ),
        ("set_treasury", json!({ "near": attacker, "evm": null })),
        ("set_timelock_delay", json!({ "delay": "0" })),
        ("set_guardian", json!({ "guardian": null })),
        ("propose_change", json!({ "change": "Unfreeze" })),
        ("execute_change", json!({ "id": id })),
    ];
    for (method, args) in attempts {
        assert_reverts(
            f.call(guardian, method, args).await,
            "Only the owner can call this method",
        );
    }
    assert_eq!(f.view::<Value>("get_config", json!({})).await, before);
    // Refused calls pay the contract a share of their gas, so its balance can
    // only have risen: nothing left it.
    assert!(
        balance(f.contract.as_account()).await >= contract_before,
        "the contract's funds did not move"
    );
    assert!(balance(guardian).await <= balance_before, "only gas spent");

    // The stop-powers work.
    f.call(guardian, "cancel_change", json!({ "id": id }))
        .await
        .into_result()
        .unwrap();
    assert!(pending(&f).await.is_empty());
    f.call(guardian, "freeze", json!({}))
        .await
        .into_result()
        .unwrap();
    assert_eq!(f.dry_run(near(), 1).await, deny(minder::DenyReason::Frozen));
    f.call(guardian, "revoke_mandate", json!({}))
        .await
        .into_result()
        .unwrap();
    let mandate: Value = f.view("get_mandate", json!({})).await;
    assert_eq!(mandate["revoked"], true);

    // A stranger gets none of the three.
    for method in ["freeze", "revoke_mandate"] {
        assert_reverts(
            f.call(&f.recipient, method, json!({})).await,
            "Only the owner or the guardian",
        );
    }
}

/// Adding a guardian is instant; removing it is held for the delay, and the
/// guardian cannot cancel its own removal, however it tries. The owner can.
#[tokio::test]
async fn a_guardian_removal_waits_out_the_delay_and_the_guardian_cannot_stop_it() {
    let f = timelocked().await;
    f.owner_ok("set_guardian", json!({ "guardian": f.outsider.id() }))
        .await;
    let config: Value = f.view("get_config", json!({})).await;
    assert_eq!(config["guardian"], json!(f.outsider.id()));

    assert_reverts(
        f.call(&f.owner, "set_guardian", json!({ "guardian": null }))
            .await,
        "Timelocked",
    );
    let removal = json!({ "SetGuardian": { "guardian": null } });
    let id: String = f
        .call(&f.owner, "propose_change", json!({ "change": removal }))
        .await
        .into_result()
        .unwrap()
        .json()
        .unwrap();
    assert_reverts(
        f.call(&f.outsider, "cancel_change", json!({ "id": id }))
            .await,
        "The guardian cannot cancel a change to the guardian",
    );
    // The guardian does what it can in the meantime.
    f.call(&f.outsider, "freeze", json!({}))
        .await
        .into_result()
        .unwrap();
    assert_reverts(
        f.call(&f.owner, "execute_change", json!({ "id": id }))
            .await,
        "Not ready",
    );
    let ready_at: u64 = pending(&f).await[0]["ready_at"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    f.advance_to(ready_at).await;
    f.call(&f.owner, "execute_change", json!({ "id": id }))
        .await
        .into_result()
        .unwrap();
    let config: Value = f.view("get_config", json!({})).await;
    assert_eq!(config["guardian"], Value::Null);
    assert_reverts(
        f.call(&f.outsider, "freeze", json!({})).await,
        "Only the owner or the guardian",
    );
}

/// A standoff cannot lose funds: with the guardian having frozen and revoked,
/// the owner still cannot send anywhere but the treasury at once, and can
/// always send there.
#[tokio::test]
async fn a_standoff_between_owner_and_guardian_cannot_lose_funds() {
    let f = timelocked().await;
    f.owner_ok("set_guardian", json!({ "guardian": f.outsider.id() }))
        .await;
    for method in ["freeze", "revoke_mandate"] {
        f.call(&f.outsider, method, json!({}))
            .await
            .into_result()
            .unwrap();
    }
    let stranger = f.worker.dev_create_account().await.unwrap();
    let before = balance(&stranger).await;
    assert_reverts(
        f.call(
            &f.owner,
            "owner_withdraw",
            json!({ "asset": near(), "amount": "1000", "to": stranger.id() }),
        )
        .await,
        "Timelocked",
    );
    assert_eq!(balance(&stranger).await, before);

    let treasury_before = balance(&f.recipient).await;
    f.owner_ok(
        "owner_withdraw",
        json!({ "asset": near(), "amount": "1000", "to": f.recipient.id() }),
    )
    .await;
    assert_eq!(balance(&f.recipient).await, treasury_before + 1000);
}

// ---- The budget period ----

/// The bypass this closes. With the budget used up, a compromised owner key
/// revokes and grants again, hoping for a fresh budget. The re-grant is instant
/// and does not refresh it; asking for the reset is refused as timelocked, and
/// takes the delay when proposed.
#[tokio::test]
async fn a_compromised_owner_cannot_reset_a_used_budget_at_once() {
    // Armed with the budget first (raising a window later would itself be
    // timelocked), and only then the timelock switched on.
    let f = Fixture::armed().await;
    f.owner_ok(
        "set_asset_policy",
        json!({
            "asset": near(), "per_tx_cap": "50", "window_limit": "1000", "epoch_budget": "70",
        }),
    )
    .await;
    f.owner_ok("set_timelock_delay", json!({ "delay": DELAY.to_string() }))
        .await;
    for amount in [40, 30] {
        let result = f.request_transfer(near(), amount, f.recipient.id()).await;
        assert!(result.is_success(), "{result:?}");
    }
    assert_eq!(
        f.dry_run(near(), 1).await,
        deny(minder::DenyReason::MandateBudgetExceeded)
    );

    // The instant route: revoke, and a new key for the same budget period.
    f.owner_ok("revoke_mandate", json!({})).await;
    let rotated = fresh_key();
    let expiry = f.now().await + 3_600 * SECOND;
    f.grant(&rotated, expiry).await;
    assert_eq!(
        f.dry_run_key(&rotated, near(), 1).await,
        deny(minder::DenyReason::MandateBudgetExceeded),
        "an instant re-grant did not refresh the budget"
    );

    // Asking for the reset is a loosening: refused when called ...
    let reset_key = fresh_key();
    assert_reverts(
        f.call(
            &f.owner,
            "grant_mandate",
            json!({ "agent_key": reset_key, "expiry": expiry.to_string(), "reset_budget": true }),
        )
        .await,
        "Timelocked",
    );
    assert_eq!(
        f.dry_run_key(&rotated, near(), 1).await,
        deny(minder::DenyReason::MandateBudgetExceeded)
    );

    // ... and held for the delay when proposed.
    let change = json!({ "GrantMandate": {
        "agent_key": reset_key, "expiry": expiry.to_string(), "scope": null, "reset_budget": true,
    } });
    let id: String = f
        .call(&f.owner, "propose_change", json!({ "change": change }))
        .await
        .into_result()
        .unwrap()
        .json()
        .unwrap();
    assert_reverts(
        f.call(&f.owner, "execute_change", json!({ "id": id }))
            .await,
        "Not ready",
    );
    let ready_at: u64 = pending(&f).await[0]["ready_at"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    f.advance_to(ready_at).await;
    f.call(&f.owner, "execute_change", json!({ "id": id }))
        .await
        .into_result()
        .unwrap();
    assert_eq!(
        f.dry_run_key(&reset_key, near(), 50).await,
        minder::Decision::Allow
    );
}
