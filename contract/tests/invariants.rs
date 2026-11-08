//! Each test attempts a violation and asserts the expected deny reason or
//! revert. See DESIGN.md for the invariant-to-test map.

mod common;

use common::*;
use minder::{Decision, DenyReason, Mandate, Status};
use near_sdk::serde_json::json;

// ---- Per-transaction cap ----

#[tokio::test]
async fn amount_over_per_tx_cap_is_denied() {
    let f = Fixture::armed().await;
    assert_eq!(f.dry_run(near(), PER_TX_CAP).await, Decision::Allow);
    assert_eq!(
        f.dry_run(near(), PER_TX_CAP + 1).await,
        deny(DenyReason::PerTxCapExceeded)
    );
}

// ---- Windowed cap ----

#[tokio::test]
async fn cumulative_spend_within_one_window_is_capped() {
    // No time travel: several spends land inside one window (however they
    // fall across sub-buckets) and their sum is what the limit applies to.
    let f = Fixture::armed().await;
    for amount in [30, 20, 10] {
        f.record_spend(near(), amount).await;
    }
    assert_eq!(f.live_spend(near()).await, 60);
    assert_eq!(f.dry_run(near(), 40).await, Decision::Allow);
    assert_eq!(
        f.dry_run(near(), 41).await,
        deny(DenyReason::WindowLimitExceeded)
    );
}

#[tokio::test]
async fn spend_stops_counting_once_a_full_window_has_passed() {
    // Coarse on purpose: advance well past one full window and check that the
    // old spend is gone. Exact slot boundaries are covered by the unit tests
    // in `window.rs`, where `now` is controlled.
    let f = Fixture::armed().await;
    f.record_spend(near(), 60).await;
    assert_eq!(
        f.dry_run(near(), 41).await,
        deny(DenyReason::WindowLimitExceeded)
    );

    let recorded_at = f.now().await;
    f.advance_to(recorded_at + WINDOW_LEN + 2 * SLOT_LEN).await;

    assert_eq!(f.live_spend(near()).await, 0);
    assert_eq!(f.dry_run(near(), 41).await, Decision::Allow);
}

#[tokio::test]
async fn reinstating_a_policy_keeps_spend_history() {
    let f = Fixture::armed().await;
    f.record_spend(near(), 60).await;
    f.owner_ok("remove_asset_policy", json!({ "asset": near() }))
        .await;
    f.owner_ok(
        "set_asset_policy",
        json!({ "asset": near(), "per_tx_cap": PER_TX_CAP.to_string(), "window_limit": WINDOW_LIMIT.to_string() }),
    )
    .await;
    assert_eq!(f.live_spend(near()).await, 60);
    assert_eq!(
        f.dry_run(near(), 41).await,
        deny(DenyReason::WindowLimitExceeded)
    );
}

// ---- Allowlists ----

#[tokio::test]
async fn recipient_not_on_allowlist_is_denied() {
    let f = Fixture::armed().await;
    assert_eq!(
        f.dry_run_as(f.agent.id(), near(), 1, f.outsider.id()).await,
        deny(DenyReason::RecipientNotAllowed)
    );

    f.owner_ok(
        "set_recipient",
        json!({ "recipient": f.recipient.id(), "allowed": false }),
    )
    .await;
    assert_eq!(
        f.dry_run(near(), 1).await,
        deny(DenyReason::RecipientNotAllowed)
    );
}

#[tokio::test]
async fn asset_without_a_policy_is_denied() {
    let f = Fixture::armed().await;
    assert_eq!(
        f.dry_run(nep141(), 1).await,
        deny(DenyReason::AssetNotAllowed)
    );

    f.owner_ok("remove_asset_policy", json!({ "asset": near() }))
        .await;
    assert_eq!(
        f.dry_run(near(), 1).await,
        deny(DenyReason::AssetNotAllowed)
    );
}

#[tokio::test]
async fn only_the_mandated_agent_is_authorized() {
    let f = Fixture::armed().await;
    assert_eq!(
        f.dry_run_as(f.outsider.id(), near(), 1, f.recipient.id())
            .await,
        deny(DenyReason::NotAuthorized)
    );
}

// ---- Role separation ----

#[tokio::test]
async fn owner_only_methods_revert_for_everyone_else() {
    let f = Fixture::armed().await;
    let calls = [
        (
            "set_asset_policy",
            json!({ "asset": near(), "per_tx_cap": "1", "window_limit": "1" }),
        ),
        ("remove_asset_policy", json!({ "asset": near() })),
        (
            "set_recipient",
            json!({ "recipient": f.outsider.id(), "allowed": true }),
        ),
        (
            "grant_mandate",
            json!({ "agent": f.outsider.id(), "expiry": u64::MAX.to_string() }),
        ),
        ("revoke_mandate", json!({})),
        ("freeze", json!({})),
        ("unfreeze", json!({})),
    ];
    for who in [&f.agent, &f.outsider] {
        for (method, args) in &calls {
            assert_reverts(
                f.call(who, method, args.clone()).await,
                "Only the owner can call this method",
            );
        }
    }
    // The contract account is not the owner either.
    assert_reverts(
        f.call(&f.contract.as_account().clone(), "freeze", json!({}))
            .await,
        "Only the owner can call this method",
    );
    // None of the attempts changed anything.
    assert_eq!(f.dry_run(near(), 1).await, Decision::Allow);
}

// ---- Single initialization ----

#[tokio::test]
async fn second_init_reverts() {
    let f = Fixture::new().await;
    assert_reverts(
        f.call(&f.owner, "init", f.init_args()).await,
        "The contract has already been initialized",
    );
    assert_reverts(
        f.call(&f.contract.as_account().clone(), "init", f.init_args())
            .await,
        "The contract has already been initialized",
    );
    // A takeover attempt by an outsider is refused by the same guard.
    let takeover = json!({
        "owner": f.outsider.id(),
        "window_len": WINDOW_LEN.to_string(),
        "num_buckets": NUM_BUCKETS,
    });
    assert_reverts(
        f.call(&f.outsider, "init", takeover).await,
        "The contract has already been initialized",
    );
    let config: minder::Config = f.view("get_config", json!({})).await;
    assert_eq!(&config.owner, f.owner.id());
}

#[tokio::test]
async fn init_rejects_callers_other_than_owner_or_contract_and_bad_windows() {
    let f = Fixture::deploy().await;
    let bad_caller = json!({ "owner": f.owner.id(), "window_len": WINDOW_LEN.to_string(), "num_buckets": NUM_BUCKETS });
    assert_reverts(
        f.call(&f.outsider, "init", bad_caller).await,
        "init must be called by the owner or the contract account",
    );

    for (window_len, num_buckets) in [
        (WINDOW_LEN, 0u32),
        (WINDOW_LEN, 65),
        (0, NUM_BUCKETS),
        (WINDOW_LEN + 1, NUM_BUCKETS),
    ] {
        let args = json!({ "owner": f.owner.id(), "window_len": window_len.to_string(), "num_buckets": num_buckets });
        let result = f.call(&f.owner, "init", args).await;
        assert!(
            result.is_failure(),
            "init({window_len}, {num_buckets}) should revert"
        );
    }

    // Failed inits left no state behind, so a valid one by the owner works.
    f.call(&f.owner, "init", f.init_args())
        .await
        .into_result()
        .unwrap();
}

// ---- Monotonic revocation, expiry and freeze ----

#[tokio::test]
async fn revoked_mandate_stays_revoked_until_a_new_grant() {
    let f = Fixture::armed().await;
    assert_eq!(f.dry_run(near(), 1).await, Decision::Allow);

    f.owner_ok("revoke_mandate", json!({})).await;
    assert_eq!(f.dry_run(near(), 1).await, deny(DenyReason::MandateRevoked));

    // Nothing else the owner or agent can do restores it.
    f.owner_ok("freeze", json!({})).await;
    f.owner_ok("unfreeze", json!({})).await;
    f.owner_ok(
        "set_asset_policy",
        json!({ "asset": near(), "per_tx_cap": "1000", "window_limit": "1000" }),
    )
    .await;
    assert_reverts(
        f.call(
            &f.agent,
            "grant_mandate",
            json!({ "agent": f.agent.id(), "expiry": u64::MAX.to_string() }),
        )
        .await,
        "Only the owner can call this method",
    );
    assert_eq!(f.dry_run(near(), 1).await, deny(DenyReason::MandateRevoked));

    // An explicit new grant is the only way back, under a new epoch.
    let expiry = f.now().await + 3_600 * SECOND;
    f.owner_ok(
        "grant_mandate",
        json!({ "agent": f.agent.id(), "expiry": expiry.to_string() }),
    )
    .await;
    assert_eq!(f.dry_run(near(), 1).await, Decision::Allow);
    let mandate: Option<Mandate> = f.view("get_mandate", json!({})).await;
    let mandate = mandate.unwrap();
    assert_eq!(mandate.epoch, 2);
    assert!(!mandate.revoked);
}

#[tokio::test]
async fn expired_mandate_stays_expired_until_a_new_grant() {
    let f = Fixture::armed().await;
    let expiry = f.now().await + 120 * SECOND;
    f.owner_ok(
        "grant_mandate",
        json!({ "agent": f.agent.id(), "expiry": expiry.to_string() }),
    )
    .await;
    assert_eq!(f.dry_run(near(), 1).await, Decision::Allow);

    f.advance_to(expiry + SLOT_LEN).await;
    assert_eq!(f.dry_run(near(), 1).await, deny(DenyReason::MandateExpired));

    f.owner_ok("freeze", json!({})).await;
    f.owner_ok("unfreeze", json!({})).await;
    assert_eq!(f.dry_run(near(), 1).await, deny(DenyReason::MandateExpired));

    // An already-past expiry cannot be granted.
    assert_reverts(
        f.call(
            &f.owner,
            "grant_mandate",
            json!({ "agent": f.agent.id(), "expiry": expiry.to_string() }),
        )
        .await,
        "Mandate expiry must be in the future",
    );

    let new_expiry = f.now().await + 3_600 * SECOND;
    f.owner_ok(
        "grant_mandate",
        json!({ "agent": f.agent.id(), "expiry": new_expiry.to_string() }),
    )
    .await;
    assert_eq!(f.dry_run(near(), 1).await, Decision::Allow);
}

#[tokio::test]
async fn frozen_contract_denies_everything_until_unfrozen() {
    let f = Fixture::armed().await;
    f.owner_ok("freeze", json!({})).await;
    let status: Status = f.view("get_status", json!({})).await;
    assert_eq!(status, Status::Frozen);
    assert_eq!(f.dry_run(near(), 1).await, deny(DenyReason::Frozen));

    // A fresh grant does not lift a freeze; only unfreeze does.
    let expiry = f.now().await + 3_600 * SECOND;
    f.owner_ok(
        "grant_mandate",
        json!({ "agent": f.agent.id(), "expiry": expiry.to_string() }),
    )
    .await;
    assert_eq!(f.dry_run(near(), 1).await, deny(DenyReason::Frozen));

    f.owner_ok("unfreeze", json!({})).await;
    assert_eq!(f.dry_run(near(), 1).await, Decision::Allow);
}

#[tokio::test]
async fn no_mandate_is_denied_and_revoking_nothing_reverts() {
    let f = Fixture::new().await;
    f.owner_ok(
        "set_asset_policy",
        json!({ "asset": near(), "per_tx_cap": "50", "window_limit": "100" }),
    )
    .await;
    assert_eq!(
        f.dry_run_as(f.agent.id(), near(), 1, f.recipient.id())
            .await,
        deny(DenyReason::NoMandate)
    );
    assert_reverts(
        f.call(&f.owner, "revoke_mandate", json!({})).await,
        "No mandate to revoke",
    );
}

// ---- Getters ----

#[tokio::test]
async fn getters_report_configuration() {
    let f = Fixture::armed().await;
    let config: minder::Config = f.view("get_config", json!({})).await;
    assert_eq!(&config.owner, f.owner.id());
    assert_eq!(config.window_len.0, WINDOW_LEN);
    assert_eq!(config.num_buckets, NUM_BUCKETS);
    assert_eq!(config.status, Status::Active);

    let policies: Vec<(minder::AssetId, minder::AssetPolicy)> =
        f.view("get_asset_policies", json!({})).await;
    assert_eq!(policies.len(), 1);
    assert_eq!(policies[0].1.per_tx_cap.0, PER_TX_CAP);
    assert_eq!(policies[0].1.window_limit.0, WINDOW_LIMIT);

    let recipients: Vec<near_workspaces::AccountId> = f.view("get_recipients", json!({})).await;
    assert_eq!(recipients, vec![f.recipient.id().clone()]);
    let allowed: bool = f
        .view(
            "is_recipient_allowed",
            json!({ "recipient": f.recipient.id() }),
        )
        .await;
    assert!(allowed);
}
