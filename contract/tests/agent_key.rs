//! The agent as a native function-call access key on the contract's own
//! account, proven against the real protocol in a sandbox.
//!
//! `grant_mandate` adds a key to the contract account, restricted by the
//! protocol to `request_local_transfer` and `request_evm_payment` on the
//! contract itself. These tests sign with that real key and show what the
//! runtime lets it do and what it refuses before any contract code runs, and
//! what `revoke_mandate` and a re-grant do to the key on the account.

mod common;

use common::*;
use minder::Decision;
use near_sdk::serde_json::{json, Value};
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{AccessKeyPermission, KeyType, NearToken, SecretKey};
use near_workspaces::{Account, AccountId};

const AGENT_METHODS: [&str; 3] = [
    "request_local_transfer",
    "request_evm_payment",
    "request_evm_token_payment",
];

/// What the runtime says when it refuses a transaction, or what the receipt
/// says if it was accepted and failed. A refusal at the protocol level is an
/// invalid transaction: it is never executed, so no contract code ran.
fn refusal(result: Result<ExecutionFinalResult, near_workspaces::error::Error>) -> String {
    match result {
        Err(error) => format!("{error:?}"),
        Ok(result) => match result.into_result() {
            Err(failure) => format!("EXECUTED AND FAILED: {failure:?}"),
            Ok(_) => "ACCEPTED".to_string(),
        },
    }
}

async fn call_refusal(who: &Account, receiver: &AccountId, method: &str) -> String {
    refusal(
        who.call(receiver, method)
            .args_json(json!({}))
            .transact()
            .await,
    )
}

/// Whether the refusal says the key is not on the account: the node's
/// `AccessKeyNotFound`, or the client's own `UnknownAccessKey` when it asks the
/// node for the key's nonce before signing.
fn key_is_unknown(why: &str) -> bool {
    why.contains("AccessKeyNotFound") || why.contains("UnknownAccessKey")
}

/// Every method the contract exports, from the committed surface snapshot,
/// plus the test-only seeding method the sandbox build adds.
fn all_methods() -> Vec<String> {
    let mut methods: Vec<String> = include_str!("snapshots/method-surface.txt")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| line.split_whitespace().nth(1).unwrap().to_string())
        .collect();
    methods.push("test_record_spend".to_string());
    methods
}

async fn keys(f: &Fixture) -> Vec<(near_workspaces::types::PublicKey, AccessKeyPermission)> {
    f.worker
        .view_access_keys(f.contract.id())
        .await
        .unwrap()
        .into_iter()
        .map(|info| (info.public_key, info.access_key.permission))
        .collect()
}

fn pk(text: &str) -> near_workspaces::types::PublicKey {
    text.parse().unwrap()
}

async fn has_key(f: &Fixture, key: &str) -> bool {
    let key = pk(key);
    keys(f).await.iter().any(|(candidate, _)| *candidate == key)
}

async fn request(f: &Fixture, who: &Account) -> ExecutionFinalResult {
    who.call(f.contract.id(), "request_local_transfer")
        .args_json(json!({
            "asset": near(), "amount": "1", "recipient": f.recipient.id(),
        }))
        .gas(near_workspaces::types::Gas::from_tgas(AGENT_TGAS))
        .transact()
        .await
        .unwrap()
}

async fn mandate(f: &Fixture) -> Value {
    f.view("get_mandate", json!({})).await
}

// ---- Provisioning ----

#[tokio::test]
async fn a_grant_puts_a_scoped_function_call_key_on_the_contract_account() {
    let f = Fixture::armed().await;
    let key = pk(&f.agent_key);
    let (_, permission) = keys(&f)
        .await
        .into_iter()
        .find(|(candidate, _)| *candidate == key)
        .expect("the agent key is on the contract account");
    let AccessKeyPermission::FunctionCall(permission) = permission else {
        panic!("the agent key must be a function-call key, not full access");
    };
    // The protocol-level scope: only the contract itself, only the three agent
    // methods, and a fixed gas allowance that is never unlimited.
    assert_eq!(permission.receiver_id, f.contract.id().to_string());
    assert_eq!(permission.method_names, AGENT_METHODS);
    assert_eq!(permission.allowance, Some(NearToken::from_millinear(250)));
}

// ---- What the real key can and cannot reach ----

#[tokio::test]
async fn the_protocol_refuses_the_agent_key_every_method_but_the_two_agent_ones() {
    let f = Fixture::armed().await;
    let before = (
        mandate(&f).await,
        f.view::<Value>("get_status", json!({})).await,
    );

    let mut refused = 0;
    for method in all_methods() {
        if AGENT_METHODS.contains(&method.as_str()) {
            continue;
        }
        let why = call_refusal(&f.agent, f.contract.id(), &method).await;
        assert!(
            why.contains("MethodNameMismatch"),
            "{method}: the protocol must refuse it as a method name mismatch, got {why}"
        );
        refused += 1;
    }
    // Owner methods, views, callbacks, init: everything except the three.
    assert!(refused > 25, "only {refused} methods were exercised");

    // And no contract logic ran for any of them.
    let after = (
        mandate(&f).await,
        f.view::<Value>("get_status", json!({})).await,
    );
    assert_eq!(before, after);
}

#[tokio::test]
async fn the_agent_key_cannot_call_another_contract_such_as_the_signer() {
    let f = Fixture::armed().await;
    let signer = f.deploy_signer().await;
    let why = refusal(
        f.agent
            .call(signer.id(), "sign")
            .args_json(json!({
                "request": { "path": "p", "payload_v2": { "Ecdsa": "00".repeat(32) }, "domain_id": 0 }
            }))
            .transact()
            .await,
    );
    assert!(why.contains("ReceiverMismatch"), "{why}");
    let requests: Vec<Value> = signer.view("get_requests").await.unwrap().json().unwrap();
    assert!(requests.is_empty(), "the signer saw nothing");
}

#[tokio::test]
async fn the_agent_key_cannot_do_anything_but_call_the_two_methods() {
    let f = Fixture::armed().await;
    let own = f.contract.id();

    // A transfer of value is not a function call.
    let transfer = refusal(
        f.agent
            .transfer_near(f.outsider.id(), NearToken::from_yoctonear(1))
            .await,
    );
    assert!(transfer.contains("RequiresFullAccess"), "{transfer}");

    // Nor are the key-management and deployment actions that would remove the fence.
    let new_key = SecretKey::from_random(KeyType::ED25519).public_key();
    let delete_self = refusal(
        f.agent
            .batch(own)
            .delete_key(pk(&f.agent_key))
            .transact()
            .await,
    );
    assert!(delete_self.contains("RequiresFullAccess"), "{delete_self}");
    let add_full_access = refusal(
        f.agent
            .batch(own)
            .add_key(new_key, near_workspaces::types::AccessKey::full_access())
            .transact()
            .await,
    );
    assert!(
        add_full_access.contains("RequiresFullAccess"),
        "{add_full_access}"
    );
    let deploy = refusal(
        f.agent
            .batch(own)
            .deploy(&[0, 97, 115, 109])
            .transact()
            .await,
    );
    assert!(deploy.contains("RequiresFullAccess"), "{deploy}");
}

#[tokio::test]
async fn the_agent_key_passes_the_new_authentication_on_both_agent_methods() {
    let f = Fixture::armed().await;

    // request_local_transfer: allowed, so it reserves and pays.
    let result = request(&f, &f.agent).await;
    assert!(result.is_success(), "{result:?}");
    assert_eq!(f.live_spend(near()).await, 1);

    // request_evm_payment: the key reaches it and is authenticated. The mock
    // signer fails the call, so nothing is signed, but the request itself was
    // accepted and took a nonce, which a denied request never does.
    let signer = f.deploy_signer().await;
    signer
        .call("set_mode")
        .args_json(json!({ "mode": "Fail" }))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    f.set_policy(
        json!({ "Evm": 11155111 }),
        100_000_000_000_000,
        120_000_000_000_000,
    )
    .await;
    f.owner_ok(
        "set_evm_recipient",
        json!({ "address": "0x70997970c51812dc3a010c7d01b50e0d17dc79c8", "allowed": true }),
    )
    .await;
    f.owner_ok(
        "set_evm_config",
        json!({
            "signer_id": signer.id(), "derivation_path": "minder-evm-1",
            "sender": fixture_sender(), "chain_id": 11155111,
            "max_fee_per_gas": "2000000000", "max_priority_fee_per_gas": "1000000000",
            "gas_limit": null,
        }),
    )
    .await;
    let result = f
        .agent
        .call(f.contract.id(), "request_evm_payment")
        .args_json(json!({
            "recipient": "0x70997970c51812dc3a010c7d01b50e0d17dc79c8", "amount": "1000",
        }))
        .gas(near_workspaces::types::Gas::from_tgas(AGENT_TGAS))
        .transact()
        .await
        .unwrap();
    assert!(result.is_success(), "{result:?}");
    let nonce: String = f
        .view("get_evm_nonce", json!({ "chain_id": 11155111 }))
        .await;
    assert_eq!(nonce, "1", "the request was authenticated and took a nonce");
}

#[tokio::test]
async fn another_key_on_the_contract_account_is_not_the_agent() {
    // A second function-call key with the very same scope, but not the
    // mandate's: the contract authenticates the key, not the account.
    let f = Fixture::armed().await;
    let stray = SecretKey::from_random(KeyType::ED25519);
    f.contract
        .as_account()
        .batch(f.contract.id())
        .add_key(
            stray.public_key(),
            near_workspaces::types::AccessKey::function_call_access(
                f.contract.id(),
                &AGENT_METHODS,
                None,
            ),
        )
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let stray_account = Account::from_secret_key(f.contract.id().clone(), stray, &f.worker);
    let result = request(&f, &stray_account).await;
    assert_reverts(result, "Denied: NotAuthorized");
}

// ---- Revocation ----

#[tokio::test]
async fn revoking_deletes_the_key_and_the_deleted_key_can_no_longer_reach_the_contract() {
    let f = Fixture::armed().await;
    assert!(has_key(&f, &f.agent_key).await);
    assert!(request(&f, &f.agent).await.is_success());

    f.owner_ok("revoke_mandate", json!({})).await;

    assert!(!has_key(&f, &f.agent_key).await, "the key is gone");
    let m = mandate(&f).await;
    assert_eq!(m["revoked"], true);
    // The protocol refuses the deleted key outright.
    let why = refusal(
        f.agent
            .call(f.contract.id(), "request_local_transfer")
            .args_json(json!({ "asset": near(), "amount": "1", "recipient": f.recipient.id() }))
            .transact()
            .await,
    );
    assert!(key_is_unknown(&why), "{why}");
}

#[tokio::test]
async fn after_a_revoke_the_epoch_guard_denies_even_if_the_key_survived_the_delete() {
    // The delete is a separate receipt that may land late or fail. Simulate
    // the key surviving: put it back with the deployer key. The call now gets
    // through the protocol, and the contract's own flag still refuses it.
    let f = Fixture::armed().await;
    f.owner_ok("revoke_mandate", json!({})).await;
    assert!(!has_key(&f, &f.agent_key).await);
    f.contract
        .as_account()
        .batch(f.contract.id())
        .add_key(
            pk(&f.agent_key),
            near_workspaces::types::AccessKey::function_call_access(
                f.contract.id(),
                &AGENT_METHODS,
                None,
            ),
        )
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    assert!(has_key(&f, &f.agent_key).await);

    assert_reverts(request(&f, &f.agent).await, "Denied: MandateRevoked");
    assert_eq!(f.live_spend(near()).await, 0);
    assert_eq!(f.reserved(near()).await, 0);
}

#[tokio::test]
async fn revoking_when_the_key_is_already_gone_does_not_fail() {
    // Remove the key behind the contract's back, then revoke: the deletion
    // receipt fails, and the revoke call itself must still succeed.
    let f = Fixture::armed().await;
    f.contract
        .as_account()
        .batch(f.contract.id())
        .delete_key(pk(&f.agent_key))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    f.owner_ok("revoke_mandate", json!({})).await;
    assert_eq!(mandate(&f).await["revoked"], true);
}

// ---- Re-granting ----

#[tokio::test]
async fn granting_after_a_revoke_reissues_a_fresh_key_and_epoch() {
    let f = Fixture::armed().await;
    f.owner_ok("revoke_mandate", json!({})).await;

    let new_key = SecretKey::from_random(KeyType::ED25519);
    let expiry = f.now().await + 3_600 * SECOND;
    f.grant(&new_key.public_key().to_string(), expiry).await;

    let m = mandate(&f).await;
    assert_eq!(
        (m["epoch"].as_u64(), m["revoked"].as_bool()),
        (Some(2), Some(false))
    );
    assert!(has_key(&f, &new_key.public_key().to_string()).await);
    assert!(!has_key(&f, &f.agent_key).await);

    // The new key works; the old one is refused by the protocol.
    let new_agent = Account::from_secret_key(f.contract.id().clone(), new_key, &f.worker);
    assert!(request(&f, &new_agent).await.is_success());
    let why = refusal(
        f.agent
            .call(f.contract.id(), "request_local_transfer")
            .args_json(json!({}))
            .transact()
            .await,
    );
    assert!(key_is_unknown(&why), "{why}");
}

#[tokio::test]
async fn replacing_a_live_mandate_swaps_the_keys_on_the_account() {
    let f = Fixture::armed().await;
    let new_key = SecretKey::from_random(KeyType::ED25519);
    let expiry = f.now().await + 3_600 * SECOND;
    f.grant(&new_key.public_key().to_string(), expiry).await;

    // The old agent's key is deleted in a receipt of its own, the new one
    // added in another; afterwards exactly the new one is on the account.
    assert!(!has_key(&f, &f.agent_key).await);
    assert!(has_key(&f, &new_key.public_key().to_string()).await);
    assert_eq!(mandate(&f).await["epoch"], 2);
}

#[tokio::test]
async fn the_key_of_a_live_mandate_cannot_be_granted_again() {
    let f = Fixture::armed().await;
    let expiry = f.now().await + 3_600 * SECOND;
    assert_reverts(
        f.call(
            &f.owner,
            "grant_mandate",
            json!({ "agent_key": f.agent_key, "expiry": expiry.to_string() }),
        )
        .await,
        "current agent key",
    );
    assert_eq!(mandate(&f).await["epoch"], 1);
}

// ---- Expiry ----

#[tokio::test]
async fn an_expired_mandate_denies_through_evaluate_while_the_key_stays_on_the_account() {
    let f = Fixture::new().await;
    f.set_policy(near(), PER_TX_CAP, WINDOW_LIMIT).await;
    f.owner_ok(
        "set_recipient",
        json!({ "recipient": f.recipient.id(), "allowed": true }),
    )
    .await;
    let expiry = f.now().await + 120 * SECOND;
    f.grant(&f.agent_key, expiry).await;
    assert!(request(&f, &f.agent).await.is_success());

    f.advance_to(expiry + SLOT_LEN).await;
    assert_reverts(request(&f, &f.agent).await, "Denied: MandateExpired");
    assert!(
        has_key(&f, &f.agent_key).await,
        "expiry does not remove the key"
    );
    assert_eq!(
        f.dry_run(near(), 1).await,
        deny(minder::DenyReason::MandateExpired)
    );
    let _: Decision = f.dry_run(near(), 1).await;
}

// ---- Deployment ----

#[tokio::test]
async fn init_rejects_the_contract_account_as_the_owner() {
    let f = Fixture::deploy().await;
    let result = f
        .contract
        .call("init")
        .args_json(json!({
            "owner": f.contract.id(), "window_len": WINDOW_LEN.to_string(),
            "num_buckets": NUM_BUCKETS,
        }))
        .transact()
        .await
        .unwrap();
    assert_reverts(result, "separate account");
}

#[tokio::test]
async fn removing_the_deployer_key_leaves_only_the_agent_key_and_control_stays_with_the_owner() {
    let f = Fixture::armed().await;

    // Before: the deployer's full-access key is on the account, and with it the
    // private callbacks are reachable (the guard only checks the predecessor).
    let deployer = f.contract.as_account().secret_key().public_key();
    let full_access = |all: &[(near_workspaces::types::PublicKey, AccessKeyPermission)]| {
        all.iter()
            .filter(|(_, permission)| matches!(permission, AccessKeyPermission::FullAccess))
            .count()
    };
    assert_eq!(full_access(&keys(&f).await), 1);
    let before = f
        .contract
        .call("on_transfer_complete")
        .args_json(json!({ "asset": near(), "amount": "1" }))
        .transact()
        .await
        .unwrap();
    assert!(
        !format!("{before:?}").contains("is private"),
        "with a full-access key on the account the private guard does not stop its holder"
    );

    // The deployment step: remove the deployer's full-access key.
    f.contract
        .as_account()
        .batch(f.contract.id())
        .delete_key(deployer)
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();

    // After: no full-access key remains, so the account holds exactly the
    // agent's scoped key and nothing else can act as the contract.
    let after = keys(&f).await;
    assert_eq!(full_access(&after), 0);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].0, pk(&f.agent_key));

    // Owner control is intact and independent of the removed key.
    f.owner_ok("freeze", json!({})).await;
    f.owner_ok("unfreeze", json!({})).await;
    let new_key = SecretKey::from_random(KeyType::ED25519);
    let expiry = f.now().await + 3_600 * SECOND;
    f.grant(&new_key.public_key().to_string(), expiry).await;
    assert!(has_key(&f, &new_key.public_key().to_string()).await);
    // And the agent still works within its scope.
    let new_agent = Account::from_secret_key(f.contract.id().clone(), new_key, &f.worker);
    assert!(request(&f, &new_agent).await.is_success());
}

// ---- Scope: least privilege within the global policy ----

#[tokio::test]
async fn a_scoped_mandate_confines_the_real_key_to_less_than_the_global_policy() {
    let f = Fixture::armed().await;
    // Globally allowed: a second recipient and the token, at the default caps.
    f.owner_ok(
        "set_recipient",
        json!({ "recipient": f.outsider.id(), "allowed": true }),
    )
    .await;
    f.set_policy(nep141(), PER_TX_CAP, WINDOW_LIMIT).await;

    // A mandate for a new key, scoped to NEAR, one recipient and a cap of 20.
    let scoped_key = SecretKey::from_random(KeyType::ED25519);
    let key = scoped_key.public_key().to_string();
    let expiry = f.now().await + 3_600 * SECOND;
    f.owner_ok(
        "grant_mandate",
        json!({
            "agent_key": key, "expiry": expiry.to_string(),
            "scope": {
                "allowed_assets": [near()],
                "allowed_near_recipients": [f.recipient.id()],
                "per_tx_cap": "20",
            },
        }),
    )
    .await;
    let m = mandate(&f).await;
    assert_eq!(m["scope"]["per_tx_cap"], "20");
    assert!(m["scope"]["allowed_evm_recipients"].is_null());
    let agent = Account::from_secret_key(f.contract.id().clone(), scoped_key, &f.worker);
    let ask = |asset: Value, amount: u128, to: AccountId| {
        let (f, agent) = (&f, &agent);
        async move {
            let viewed = f
                .dry_run_as(&key_of(agent), asset.clone(), amount, &to)
                .await;
            let result = agent
                .call(f.contract.id(), "request_local_transfer")
                .args_json(json!({ "asset": asset, "amount": amount.to_string(), "recipient": to }))
                .gas(near_workspaces::types::Gas::from_tgas(AGENT_TGAS))
                .transact()
                .await
                .unwrap();
            (viewed, result)
        }
    };

    // In scope: allowed, and the view agrees.
    let (viewed, result) = ask(near(), 20, f.recipient.id().clone()).await;
    assert_eq!(viewed, Decision::Allow);
    assert!(result.is_success(), "{result:?}");

    // Allowed globally, outside the scope: refused with the reason, and the view said so.
    let (viewed, result) = ask(near(), 5, f.outsider.id().clone()).await;
    assert_eq!(viewed, deny(minder::DenyReason::RecipientOutOfScope));
    assert_reverts(result, "Denied: RecipientOutOfScope");
    let (viewed, result) = ask(nep141(), 5, f.recipient.id().clone()).await;
    assert_eq!(viewed, deny(minder::DenyReason::AssetOutOfScope));
    assert_reverts(result, "Denied: AssetOutOfScope");
    // Under the global cap (50), over the mandate's (20).
    let (viewed, result) = ask(near(), 21, f.recipient.id().clone()).await;
    assert_eq!(viewed, deny(minder::DenyReason::PerTxCapExceeded));
    assert_reverts(result, "Denied: PerTxCapExceeded");

    // A refused request changed nothing: only the first 20 is reserved or spent.
    assert_eq!(f.reserved(near()).await + f.live_spend(near()).await, 20);
}

fn key_of(agent: &Account) -> String {
    agent.secret_key().public_key().to_string()
}

// ---- The per-grant lifetime budget ----

#[tokio::test]
async fn a_budget_bounds_what_one_grant_can_spend_and_a_new_grant_starts_afresh() {
    let f = Fixture::armed().await;
    // A window that never binds here, so the budget alone is on test.
    f.owner_ok(
        "set_asset_policy",
        json!({
            "asset": near(), "per_tx_cap": "50", "window_limit": "1000", "epoch_budget": "70",
        }),
    )
    .await;
    let policy: Value = f.view("get_asset_policy", json!({ "asset": near() })).await;
    assert_eq!(policy["epoch_budget"], "70");

    // 40 + 30 uses the whole budget; one more is refused, though the window has room.
    for amount in [40, 30] {
        let result = request_amount(&f, &f.agent, amount).await;
        assert!(result.is_success(), "{result:?}");
    }
    assert_eq!(f.live_spend(near()).await, 70);
    assert_eq!(
        f.dry_run(near(), 1).await,
        deny(minder::DenyReason::MandateBudgetExceeded)
    );
    assert_reverts(
        request_amount(&f, &f.agent, 1).await,
        "Denied: MandateBudgetExceeded",
    );

    // A new grant (revoke, then a new key) is a new epoch: a fresh budget,
    // with nothing written to reset it, while the window still holds the 70.
    f.owner_ok("revoke_mandate", json!({})).await;
    let new_key = SecretKey::from_random(KeyType::ED25519);
    let expiry = f.now().await + 3_600 * SECOND;
    f.grant(&new_key.public_key().to_string(), expiry).await;
    assert_eq!(f.live_spend(near()).await, 70);
    let new_agent = Account::from_secret_key(f.contract.id().clone(), new_key, &f.worker);
    for amount in [40, 30] {
        let result = request_amount(&f, &new_agent, amount).await;
        assert!(result.is_success(), "{result:?}");
    }
    assert_reverts(
        request_amount(&f, &new_agent, 1).await,
        "Denied: MandateBudgetExceeded",
    );
}

async fn request_amount(f: &Fixture, who: &Account, amount: u128) -> ExecutionFinalResult {
    who.call(f.contract.id(), "request_local_transfer")
        .args_json(json!({
            "asset": near(), "amount": amount.to_string(), "recipient": f.recipient.id(),
        }))
        .gas(near_workspaces::types::Gas::from_tgas(AGENT_TGAS))
        .transact()
        .await
        .unwrap()
}
