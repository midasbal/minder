//! Unit tests for the agent's mandate under the mocked NEAR environment: the
//! access key `grant_mandate` provisions and `revoke_mandate` deletes, and the
//! change from authenticating an account to authenticating a key.

use std::panic::{catch_unwind, AssertUnwindSafe};

use near_sdk::json_types::{U128, U64};
use near_sdk::mock::MockAction;
use near_sdk::test_utils::get_created_receipts;
use near_sdk::{testing_env, NearToken};

use crate::test_support::*;
use crate::{AssetId, Decision, DenyReason, Minder};

/// What a key action looks like in the mocked receipts.
#[derive(Debug, PartialEq, Eq)]
enum KeyAction {
    Add {
        key: String,
        receiver: String,
        methods: Vec<String>,
        allowance: Option<NearToken>,
    },
    Delete {
        key: String,
    },
}

fn text(key: &near_sdk::PublicKey) -> String {
    near_sdk::serde_json::to_value(key)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

/// The key actions aimed at the contract's own account, in creation order.
fn key_actions() -> Vec<KeyAction> {
    get_created_receipts()
        .iter()
        .filter(|receipt| receipt.receiver_id == account("minder.near"))
        .flat_map(|receipt| &receipt.actions)
        .filter_map(|action| match action {
            MockAction::AddKeyWithFunctionCall {
                public_key,
                allowance,
                receiver_id,
                method_names,
                ..
            } => Some(KeyAction::Add {
                key: public_key.to_string(),
                receiver: receiver_id.to_string(),
                methods: method_names.to_vec(),
                allowance: *allowance,
            }),
            MockAction::DeleteKey { public_key, .. } => Some(KeyAction::Delete {
                key: public_key.to_string(),
            }),
            _ => None,
        })
        .collect()
}

fn as_owner() {
    testing_env!(context("owner.near").build());
}

fn fresh() -> Minder {
    testing_env!(context("owner.near").build());
    let mut minder = Minder::init(account("owner.near"), U64(400), 4, None, None, None, None);
    minder.set_asset_policy(AssetId::Near, U128(PER_TX_CAP), U128(WINDOW_LIMIT), None);
    minder.set_recipient(account("bob.near"), true);
    minder
}

fn panic_message(result: std::thread::Result<()>) -> String {
    let payload = result.expect_err("expected a panic");
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

fn ask(minder: &Minder, key: near_sdk::PublicKey) -> Decision {
    minder.dry_run(key, AssetId::Near, U128(10), account("bob.near"))
}

// ---- Provisioning ----

#[test]
fn a_grant_provisions_a_function_call_key_limited_to_the_two_agent_methods() {
    let mut minder = fresh();
    minder.grant_mandate(agent_key(), U64(u64::MAX), None, None);

    assert_eq!(
        key_actions(),
        vec![KeyAction::Add {
            key: text(&agent_key()),
            // On the contract's own account, callable only on the contract itself.
            receiver: "minder.near".to_string(),
            methods: vec![
                "request_local_transfer".to_string(),
                "request_evm_payment".to_string(),
                "request_evm_token_payment".to_string()
            ],
            // A fixed, limited gas allowance: never unlimited.
            allowance: Some(NearToken::from_millinear(250)),
        }]
    );
    let mandate = minder.get_mandate().unwrap();
    assert_eq!(mandate.agent_key, agent_key());
    assert_eq!((mandate.epoch, mandate.revoked), (1, false));
}

#[test]
fn the_key_can_reach_only_methods_that_exist_on_the_contract() {
    // Each name the key is limited to is a real agent entry point, so the list
    // cannot silently drift from the methods it is meant to name.
    let abi = include_str!("../tests/snapshots/method-surface.txt");
    for method in crate::AGENT_METHODS.split(',') {
        assert!(
            abi.lines().any(|line| line == format!("call {method}")),
            "{method} is not a plain call method in the surface snapshot"
        );
    }
    assert_eq!(crate::AGENT_METHODS.split(',').count(), 3);
}

#[test]
fn only_the_owner_can_grant_or_revoke() {
    let mut minder = fresh();
    testing_env!(agent_context().build());
    let grant = catch_unwind(AssertUnwindSafe(|| {
        minder.grant_mandate(agent_key(), U64(u64::MAX), None, None)
    }));
    assert!(panic_message(grant).contains("Only the owner"));
    assert!(key_actions().is_empty());
    let revoke = catch_unwind(AssertUnwindSafe(|| minder.revoke_mandate()));
    assert!(panic_message(revoke).contains("Only the owner"));
}

#[test]
fn a_grant_in_the_past_provisions_nothing() {
    let mut minder = fresh();
    let result = catch_unwind(AssertUnwindSafe(|| {
        minder.grant_mandate(agent_key(), U64(1), None, None)
    }));
    assert!(panic_message(result).contains("must be in the future"));
    assert!(key_actions().is_empty());
    assert!(minder.get_mandate().is_none());
}

// ---- Revocation ----

#[test]
fn revoking_flags_the_mandate_and_deletes_the_key() {
    let mut minder = fresh();
    minder.grant_mandate(agent_key(), U64(u64::MAX), None, None);
    as_owner();
    minder.revoke_mandate();

    assert_eq!(
        key_actions(),
        vec![KeyAction::Delete {
            key: text(&agent_key())
        }]
    );
    assert!(minder.get_mandate().unwrap().revoked);
}

#[test]
fn a_request_after_revoking_is_denied_by_the_flag_with_no_deletion_landed() {
    // The unit environment never executes the deletion receipt, which is the
    // in-flight case: the key still exists, and the flag alone refuses it.
    let mut minder = armed_as_agent();
    testing_env!(context("owner.near").build());
    minder.revoke_mandate();
    testing_env!(agent_context().build());

    assert_eq!(
        ask(&minder, agent_key()),
        Decision::Deny(DenyReason::MandateRevoked)
    );
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _ = minder.request_local_transfer(AssetId::Near, U128(10), account("bob.near"));
    }));
    assert!(panic_message(result).contains("Denied: MandateRevoked"));
    assert!(
        get_created_receipts().is_empty(),
        "a denied request dispatches nothing"
    );
}

#[test]
fn revoking_twice_is_a_no_op_and_issues_no_second_deletion() {
    let mut minder = fresh();
    minder.grant_mandate(agent_key(), U64(u64::MAX), None, None);
    as_owner();
    minder.revoke_mandate();
    testing_env!(context("owner.near").build());
    minder.revoke_mandate();
    assert!(key_actions().is_empty());
    assert!(minder.get_mandate().unwrap().revoked);
}

#[test]
fn revoking_without_a_mandate_reverts() {
    let mut minder = fresh();
    let result = catch_unwind(AssertUnwindSafe(|| minder.revoke_mandate()));
    assert!(panic_message(result).contains("No mandate to revoke"));
}

// ---- Re-granting ----

#[test]
fn granting_after_a_revocation_provisions_a_fresh_key_under_a_new_epoch() {
    let mut minder = fresh();
    minder.grant_mandate(agent_key(), U64(u64::MAX), None, None);
    as_owner();
    minder.revoke_mandate();

    testing_env!(context("owner.near").build());
    minder.grant_mandate(other_key(), U64(u64::MAX), None, None);
    // The old key was deleted at revocation; nothing is deleted again.
    assert_eq!(
        key_actions(),
        vec![KeyAction::Add {
            key: text(&other_key()),
            receiver: "minder.near".to_string(),
            methods: vec![
                "request_local_transfer".to_string(),
                "request_evm_payment".to_string(),
                "request_evm_token_payment".to_string()
            ],
            allowance: Some(NearToken::from_millinear(250)),
        }]
    );
    let mandate = minder.get_mandate().unwrap();
    assert_eq!(
        (mandate.agent_key, mandate.epoch, mandate.revoked),
        (other_key(), 2, false)
    );
    // The new key is authorized, the old one is not.
    assert_eq!(ask(&minder, other_key()), Decision::Allow);
    assert_eq!(
        ask(&minder, agent_key()),
        Decision::Deny(DenyReason::NotAuthorized)
    );
}

#[test]
fn the_revoked_key_can_be_granted_again_once_revoked() {
    let mut minder = fresh();
    minder.grant_mandate(agent_key(), U64(u64::MAX), None, None);
    as_owner();
    minder.revoke_mandate();
    testing_env!(context("owner.near").build());
    minder.grant_mandate(agent_key(), U64(u64::MAX), None, None);
    assert_eq!(minder.get_mandate().unwrap().epoch, 2);
    assert_eq!(key_actions().len(), 1);
}

#[test]
fn replacing_a_live_mandate_deletes_the_old_key_and_adds_the_new_one() {
    let mut minder = fresh();
    minder.grant_mandate(agent_key(), U64(u64::MAX), None, None);
    testing_env!(context("owner.near").build());
    minder.grant_mandate(other_key(), U64(u64::MAX), None, None);

    let actions = key_actions();
    // Two separate receipts, the deletion first.
    assert_eq!(actions.len(), 2);
    assert_eq!(
        actions[0],
        KeyAction::Delete {
            key: text(&agent_key())
        }
    );
    assert!(matches!(&actions[1], KeyAction::Add { key, .. } if *key == text(&other_key())));
    // Deleting and adding are not one batch: a failed deletion must not stop the add.
    let receipts = get_created_receipts();
    let on_contract = receipts
        .iter()
        .filter(|receipt| receipt.receiver_id == account("minder.near"))
        .count();
    assert_eq!(on_contract, 2, "one receipt per key action");
}

#[test]
fn the_live_key_cannot_be_granted_again() {
    let mut minder = fresh();
    minder.grant_mandate(agent_key(), U64(u64::MAX), None, None);
    testing_env!(context("owner.near").build());
    let result = catch_unwind(AssertUnwindSafe(|| {
        minder.grant_mandate(agent_key(), U64(u64::MAX), None, None)
    }));
    assert!(panic_message(result).contains("current agent key"));
    assert!(key_actions().is_empty());
    assert_eq!(minder.get_mandate().unwrap().epoch, 1);
}

// ---- Authentication by key ----

#[test]
fn the_agent_is_the_holder_of_the_key_on_the_contracts_own_account() {
    let mut minder = armed_as_agent();
    let _ = minder.request_local_transfer(AssetId::Near, U128(10), account("bob.near"));
    assert_eq!(minder.get_reserved(AssetId::Near), U128(10));
}

fn request_with(
    context: &mut near_sdk::test_utils::VMContextBuilder,
    minder: &mut Minder,
) -> String {
    testing_env!(context.build());
    panic_message(catch_unwind(AssertUnwindSafe(|| {
        let _ = minder.request_local_transfer(AssetId::Near, U128(10), account("bob.near"));
    })))
}

#[test]
fn another_key_on_the_contract_account_is_not_the_agent() {
    // For instance the deployer's key, if it was never removed.
    let mut minder = armed_as_agent();
    let mut context = agent_context();
    context.signer_account_pk(other_key());
    assert!(request_with(&mut context, &mut minder).contains("Denied: NotAuthorized"));
}

#[test]
fn the_agent_key_is_not_enough_from_any_other_account() {
    // The key must reach the contract as the contract's own account. A call
    // that merely carries the key, relayed by some other account, is refused.
    let mut minder = armed_as_agent();
    for predecessor in ["agent.near", "owner.near", "bob.near"] {
        let mut context = context(predecessor);
        context
            .signer_account_id(account(predecessor))
            .signer_account_pk(agent_key());
        assert!(
            request_with(&mut context, &mut minder).contains("Denied: NotAuthorized"),
            "{predecessor}"
        );
    }
}

#[test]
fn the_old_account_style_agent_has_no_authority() {
    // An account named like the previous agent, with no key match, is nobody.
    let mut minder = armed_as_agent();
    let context = context("agent.near");
    let mut context = context;
    assert!(request_with(&mut context, &mut minder).contains("Denied: NotAuthorized"));
}

#[test]
fn an_expired_mandate_denies_even_for_its_own_key() {
    let mut minder = fresh();
    minder.grant_mandate(agent_key(), U64(2_000_000), None, None);
    let mut context = agent_context();
    context.block_timestamp(2_000_000);
    testing_env!(context.build());
    assert_eq!(
        ask(&minder, agent_key()),
        Decision::Deny(DenyReason::MandateExpired)
    );
    assert!(request_with(&mut context, &mut minder).contains("Denied: MandateExpired"));
}

// ---- init ----

#[test]
fn init_rejects_the_contract_account_as_owner() {
    testing_env!(context("minder.near").build());
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _ = Minder::init(account("minder.near"), U64(400), 4, None, None, None, None);
    }));
    assert!(panic_message(result).contains("separate account"));
}

#[test]
fn init_by_the_contract_account_for_a_separate_owner_is_fine() {
    testing_env!(context("minder.near").build());
    let minder = Minder::init(account("owner.near"), U64(400), 4, None, None, None, None);
    assert_eq!(minder.get_config().owner, account("owner.near"));
}

// ---- Scope: least privilege within the global policy ----

use crate::MandateScope;

const FAR_FUTURE: u64 = u64::MAX;

/// Armed as in the other tests (policies for NEAR and `usdc.near`, `bob.near`
/// allowed, an unscoped mandate for `agent_key()`), plus a second allowed
/// recipient, `carol.near`. The environment is left as the owner.
fn armed() -> Minder {
    let mut minder = armed_as_agent();
    as_owner();
    minder.set_recipient(account("carol.near"), true);
    minder
}

/// Replaces the mandate with one for `other_key()` carrying `scope`, then acts
/// as that key. (A live mandate cannot be re-granted to its own key.)
fn scoped_as_agent(minder: &mut Minder, scope: Option<MandateScope>) {
    as_owner();
    minder.grant_mandate(other_key(), U64(FAR_FUTURE), scope, None);
    testing_env!(agent_context_for(other_key()).build());
}

fn near_scope(
    assets: Option<Vec<AssetId>>,
    recipients: Option<Vec<&str>>,
    cap: Option<u128>,
) -> MandateScope {
    MandateScope {
        allowed_assets: assets,
        allowed_near_recipients: recipients.map(|names| names.into_iter().map(account).collect()),
        allowed_evm_recipients: None,
        per_tx_cap: cap.map(U128),
    }
}

/// What a request and the view say about the same hypothetical request.
fn outcome(minder: &mut Minder, asset: AssetId, amount: u128, to: &str) -> Decision {
    let viewed = minder.dry_run(other_key(), asset.clone(), U128(amount), account(to));
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _ = minder.request_local_transfer(asset, U128(amount), account(to));
    }));
    match result {
        Ok(()) => {
            assert_eq!(
                viewed,
                Decision::Allow,
                "the view must agree with the request"
            );
            Decision::Allow
        }
        Err(payload) => {
            let message = panic_message(Err(payload));
            let reason = [
                DenyReason::AssetNotAllowed,
                DenyReason::RecipientNotAllowed,
                DenyReason::PerTxCapExceeded,
                DenyReason::WindowLimitExceeded,
                DenyReason::AssetOutOfScope,
                DenyReason::RecipientOutOfScope,
            ]
            .into_iter()
            .find(|reason| message.contains(&format!("Denied: {reason:?}")))
            .unwrap_or_else(|| panic!("unexpected failure: {message}"));
            assert_eq!(viewed, Decision::Deny(reason), "the view must agree");
            Decision::Deny(reason)
        }
    }
}

#[test]
fn a_scoped_mandate_allows_what_is_in_scope_and_names_what_is_not() {
    let mut minder = armed();
    let scope = near_scope(Some(vec![AssetId::Near]), Some(vec!["bob.near"]), None);
    scoped_as_agent(&mut minder, Some(scope));

    assert_eq!(
        outcome(&mut minder, AssetId::Near, 10, "bob.near"),
        Decision::Allow
    );
    // Allowed globally (the token has a policy, carol is allowlisted), outside the scope.
    assert_eq!(
        outcome(&mut minder, token(), 10, "bob.near"),
        Decision::Deny(DenyReason::AssetOutOfScope)
    );
    assert_eq!(
        outcome(&mut minder, AssetId::Near, 10, "carol.near"),
        Decision::Deny(DenyReason::RecipientOutOfScope)
    );
}

#[test]
fn the_effective_cap_is_the_smaller_of_the_global_cap_and_the_mandates() {
    let mut minder = armed(); // the global per-transaction cap is 50
    scoped_as_agent(&mut minder, Some(near_scope(None, None, Some(20))));
    assert_eq!(
        outcome(&mut minder, AssetId::Near, 20, "bob.near"),
        Decision::Allow
    );
    assert_eq!(
        outcome(&mut minder, AssetId::Near, 21, "bob.near"),
        Decision::Deny(DenyReason::PerTxCapExceeded),
        "a tighter mandate cap binds"
    );

    as_owner();
    minder.revoke_mandate();
    minder.grant_mandate(
        other_key(),
        U64(FAR_FUTURE),
        Some(near_scope(None, None, Some(1_000))),
        None,
    );
    testing_env!(agent_context_for(other_key()).build());
    assert_eq!(
        outcome(&mut minder, AssetId::Near, 50, "bob.near"),
        Decision::Allow
    );
    assert_eq!(
        outcome(&mut minder, AssetId::Near, 51, "bob.near"),
        Decision::Deny(DenyReason::PerTxCapExceeded),
        "a looser mandate cap does not widen the global one"
    );
}

#[test]
fn a_scope_cannot_widen_the_global_policy() {
    let mut minder = armed();
    // In the scope, but the asset has no global policy and the recipient is not allowlisted.
    let dai = AssetId::Nep141(account("dai.near"));
    let scope = near_scope(
        Some(vec![AssetId::Near, dai.clone()]),
        Some(vec!["bob.near", "dave.near"]),
        None,
    );
    scoped_as_agent(&mut minder, Some(scope));
    assert_eq!(
        outcome(&mut minder, dai, 10, "bob.near"),
        Decision::Deny(DenyReason::AssetNotAllowed)
    );
    assert_eq!(
        outcome(&mut minder, AssetId::Near, 10, "dave.near"),
        Decision::Deny(DenyReason::RecipientNotAllowed)
    );
    // The window limit is global too: it still binds a request that is in scope.
    assert_eq!(
        outcome(&mut minder, AssetId::Near, 50, "bob.near"),
        Decision::Allow
    );
    assert_eq!(
        outcome(&mut minder, AssetId::Near, 50, "bob.near"),
        Decision::Allow
    );
    assert_eq!(
        outcome(&mut minder, AssetId::Near, 10, "bob.near"),
        Decision::Deny(DenyReason::WindowLimitExceeded)
    );
}

#[test]
fn an_unscoped_mandate_behaves_as_it_did_before_scopes() {
    for scope in [None, Some(MandateScope::default())] {
        let mut minder = armed();
        scoped_as_agent(&mut minder, scope);
        assert_eq!(minder.get_mandate().unwrap().scope, MandateScope::default());
        assert_eq!(
            outcome(&mut minder, AssetId::Near, 50, "bob.near"),
            Decision::Allow
        );
        assert_eq!(
            outcome(&mut minder, token(), 10, "carol.near"),
            Decision::Allow
        );
        assert_eq!(
            outcome(&mut minder, AssetId::Near, 51, "bob.near"),
            Decision::Deny(DenyReason::PerTxCapExceeded)
        );
        assert_eq!(
            outcome(&mut minder, AssetId::Near, 10, "dave.near"),
            Decision::Deny(DenyReason::RecipientNotAllowed)
        );
    }
}

#[test]
fn a_regrant_can_change_the_scope_and_the_new_scope_applies_from_the_new_grant() {
    let mut minder = armed();
    scoped_as_agent(
        &mut minder,
        Some(near_scope(Some(vec![AssetId::Near]), None, None)),
    );
    assert_eq!(
        outcome(&mut minder, token(), 10, "bob.near"),
        Decision::Deny(DenyReason::AssetOutOfScope)
    );

    // Re-granted (to a new key, as a live key cannot be re-granted) with a different scope.
    as_owner();
    minder.revoke_mandate();
    minder.grant_mandate(
        other_key(),
        U64(FAR_FUTURE),
        Some(near_scope(
            Some(vec![token()]),
            Some(vec!["carol.near"]),
            None,
        )),
        None,
    );
    testing_env!(agent_context_for(other_key()).build());
    assert_eq!(minder.get_mandate().unwrap().epoch, 3);
    assert_eq!(
        outcome(&mut minder, AssetId::Near, 10, "carol.near"),
        Decision::Deny(DenyReason::AssetOutOfScope),
        "the old scope is gone"
    );
    assert_eq!(
        outcome(&mut minder, token(), 10, "bob.near"),
        Decision::Deny(DenyReason::RecipientOutOfScope)
    );
    assert_eq!(
        outcome(&mut minder, token(), 10, "carol.near"),
        Decision::Allow
    );

    // And an unscoped re-grant drops the scope entirely.
    as_owner();
    minder.revoke_mandate();
    minder.grant_mandate(other_key(), U64(FAR_FUTURE), None, None);
    testing_env!(agent_context_for(other_key()).build());
    assert_eq!(
        outcome(&mut minder, AssetId::Near, 10, "bob.near"),
        Decision::Allow
    );
}

#[test]
fn a_scope_is_validated_and_stored_normalized() {
    let mut minder = armed();
    as_owner();
    let too_many = MandateScope {
        allowed_near_recipients: Some((0..65).map(|i| account(&format!("r{i}.near"))).collect()),
        ..MandateScope::default()
    };
    let result = catch_unwind(AssertUnwindSafe(|| {
        minder.grant_mandate(other_key(), U64(FAR_FUTURE), Some(too_many), None)
    }));
    assert!(panic_message(result).contains("at most 64"));

    let bad_address = MandateScope {
        allowed_evm_recipients: Some(vec!["0x1234".to_string()]),
        ..MandateScope::default()
    };
    let result = catch_unwind(AssertUnwindSafe(|| {
        minder.grant_mandate(other_key(), U64(FAR_FUTURE), Some(bad_address), None)
    }));
    assert!(panic_message(result).contains("Invalid EVM address"));
    assert_eq!(
        minder.get_mandate().unwrap().agent_key,
        agent_key(),
        "nothing was granted"
    );

    let mixed_case = MandateScope {
        allowed_evm_recipients: Some(vec![
            "0x70997970C51812dc3A010C7d01b50e0d17dc79C8".to_string()
        ]),
        ..MandateScope::default()
    };
    minder.grant_mandate(other_key(), U64(FAR_FUTURE), Some(mixed_case), None);
    assert_eq!(
        minder.get_mandate().unwrap().scope.allowed_evm_recipients,
        Some(vec![
            "0x70997970c51812dc3a010c7d01b50e0d17dc79c8".to_string()
        ])
    );
}

#[test]
fn only_the_owner_sets_a_scope_and_the_scope_is_visible() {
    let mut minder = armed();
    testing_env!(agent_context().build());
    let result = catch_unwind(AssertUnwindSafe(|| {
        minder.grant_mandate(
            other_key(),
            U64(FAR_FUTURE),
            Some(near_scope(None, None, Some(1_000_000))),
            None,
        )
    }));
    assert!(panic_message(result).contains("Only the owner"));

    as_owner();
    minder.grant_mandate(
        other_key(),
        U64(FAR_FUTURE),
        Some(near_scope(None, Some(vec!["bob.near"]), Some(7))),
        None,
    );
    let view = near_sdk::serde_json::to_value(minder.get_mandate().unwrap()).unwrap();
    assert_eq!(
        view["scope"]["allowed_near_recipients"],
        near_sdk::serde_json::json!(["bob.near"])
    );
    assert_eq!(view["scope"]["per_tx_cap"], "7");
    assert!(view["scope"]["allowed_assets"].is_null());
}
