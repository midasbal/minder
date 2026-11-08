//! Unit tests for fund custody under the mocked NEAR environment: owner
//! withdrawal and the two ways of funding the contract.

use near_sdk::json_types::U128;
use near_sdk::mock::MockAction;
use near_sdk::serde_json::json;
use near_sdk::test_utils::get_created_receipts;
use near_sdk::{testing_env, NearToken};

use crate::test_support::*;
use crate::{AssetId, Decision};

fn as_owner() {
    testing_env!(context("owner.near").build());
}

fn carol() -> near_sdk::AccountId {
    account("carol.near")
}

fn calls_method(method: &[u8]) -> bool {
    get_created_receipts().iter().any(|receipt| {
        receipt.actions.iter().any(|action| {
            matches!(action, MockAction::FunctionCallWeight { method_name, .. } if method_name == method)
        })
    })
}

// ---- owner_withdraw ----

#[test]
#[should_panic(expected = "Only the owner can call this method")]
fn only_the_owner_can_withdraw() {
    let mut minder = armed_as_agent(); // environment is the agent
    let _ = minder.owner_withdraw(AssetId::Near, U128(1), carol());
}

#[test]
fn owner_withdraw_works_while_frozen_and_after_revocation() {
    let mut minder = armed_as_agent();
    as_owner();
    minder.freeze();
    minder.revoke_mandate();

    let _ = minder.owner_withdraw(AssetId::Near, U128(25), carol());

    assert!(get_created_receipts().iter().any(|receipt| {
        receipt.receiver_id == carol() && receipt.actions.iter().any(|action| {
            matches!(
                action,
                MockAction::Transfer { deposit, .. } if *deposit == NearToken::from_yoctonear(25)
            )
        })
    }));
}

#[test]
fn native_withdraw_pays_the_recipient_and_attaches_no_callback() {
    let mut minder = armed_as_agent();
    as_owner();
    let _ = minder.owner_withdraw(AssetId::Near, U128(25), carol());

    assert!(!calls_method(b"on_transfer_complete"));
}

#[test]
fn token_withdraw_is_a_plain_ft_transfer_with_one_yocto_and_no_callback() {
    let mut minder = armed_as_agent();
    as_owner();
    let _ = minder.owner_withdraw(token(), U128(25), carol());

    let receipts = get_created_receipts();
    let transfer = function_call(&receipts, "usdc.near", "ft_transfer");
    assert_eq!(
        transfer.args,
        json!({ "receiver_id": "carol.near", "amount": "25", "memo": null })
    );
    assert_eq!(transfer.deposit, NearToken::from_yoctonear(1));
    assert!(!calls_method(b"ft_transfer_call"));
    assert!(!calls_method(b"on_transfer_complete"));
}

#[test]
fn owner_withdraw_ignores_policy() {
    let mut minder = armed_as_agent();
    as_owner();
    // Over the per-tx cap and the window limit, to an account that is not
    // allowlisted, in a token that has no policy.
    let unlisted_token = AssetId::Nep141(account("unlisted.near"));
    let amount = WINDOW_LIMIT * 10;
    let _ = minder.owner_withdraw(AssetId::Near, U128(amount), carol());
    let _ = minder.owner_withdraw(unlisted_token, U128(amount), carol());
}

#[test]
fn owner_withdraw_does_not_touch_accounting() {
    let mut minder = armed_as_agent();
    let _ = minder.request_local_transfer(AssetId::Near, U128(40), account("bob.near"));
    let dry_run = |minder: &crate::Minder| {
        minder.dry_run(
            account("agent.near"),
            AssetId::Near,
            U128(61),
            account("bob.near"),
        )
    };
    assert_eq!(reserved(&minder, AssetId::Near), 40);
    let budget_before = dry_run(&minder);

    as_owner();
    let _ = minder.owner_withdraw(AssetId::Near, U128(500), carol());
    let _ = minder.owner_withdraw(token(), U128(500), carol());

    assert_eq!(
        reserved(&minder, AssetId::Near),
        40,
        "reservation untouched"
    );
    assert_eq!(reserved(&minder, token()), 0);
    assert_eq!(committed(&minder, AssetId::Near), 0);
    assert_eq!(committed(&minder, token()), 0);
    assert_eq!(dry_run(&minder), budget_before);
}

// ---- Funding ----

#[test]
fn deposit_accepts_an_attached_amount_from_anyone() {
    let mut minder = armed_as_agent();
    testing_env!(context("anyone.near")
        .attached_deposit(NearToken::from_yoctonear(5))
        .build());
    minder.deposit();
}

#[test]
#[should_panic(expected = "Attach a deposit")]
fn deposit_without_an_attached_amount_reverts() {
    let mut minder = armed_as_agent();
    minder.deposit();
}

#[test]
fn ft_on_transfer_accepts_an_allowlisted_token_in_full() {
    let minder = armed_as_agent();
    testing_env!(context("usdc.near").build());
    // Returns the unused amount, so 0 means everything is kept.
    assert_eq!(
        minder.ft_on_transfer(account("alice.near"), U128(70), String::new()),
        U128(0)
    );
}

#[test]
fn ft_on_transfer_refunds_a_token_without_a_policy_in_full() {
    let minder = armed_as_agent();
    testing_env!(context("unlisted.near").build());
    assert_eq!(
        minder.ft_on_transfer(account("alice.near"), U128(70), String::new()),
        U128(70)
    );
}

#[test]
fn ft_on_transfer_keys_on_the_calling_token_not_the_claimed_sender() {
    let minder = armed_as_agent();
    // An account that merely names an allowlisted token as sender gains nothing.
    testing_env!(context("mallory.near").build());
    assert_eq!(
        minder.ft_on_transfer(account("usdc.near"), U128(70), String::new()),
        U128(70)
    );
}

#[test]
fn ft_on_transfer_does_not_change_accounting() {
    let mut minder = armed_as_agent();
    let _ = minder.request_local_transfer(token(), U128(30), account("bob.near"));
    testing_env!(context("usdc.near").build());
    let _ = minder.ft_on_transfer(account("alice.near"), U128(70), String::new());

    assert_eq!(reserved(&minder, token()), 30);
    assert_eq!(committed(&minder, token()), 0);
    assert_eq!(
        minder.dry_run(
            account("agent.near"),
            token(),
            U128(50),
            account("bob.near")
        ),
        Decision::Allow
    );
}
