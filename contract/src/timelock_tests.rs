//! Unit tests for the asymmetric timelock under the mocked NEAR environment:
//! what takes effect at once, what is held back for the delay and goes through
//! propose, execute and cancel, and what a compromised owner key can and
//! cannot do.

use std::panic::{catch_unwind, AssertUnwindSafe};

use near_sdk::json_types::{U128, U64};
use near_sdk::test_utils::{get_created_receipts, get_logs};
use near_sdk::testing_env;

use crate::test_support::*;
use crate::{AssetId, MandateScope, Minder, OwnerChange, Status};

/// The timelock delay of these tests, in ns.
const DELAY: u64 = 10_000;
/// The mocked block time everything starts at (see `test_support::context`).
const T0: u64 = 1_000_000;
const TREASURY: &str = "treasury.near";
const EVM_TREASURY: &str = "0x00000000000000000000000000000000000000aa";
const STRANGER: &str = "0x00000000000000000000000000000000000000bb";
const ATTACKER_EVM: &str = "0x00000000000000000000000000000000000000cc";
const TOKEN: &str = "0x1c7d4b196cb0c7b01d743fbc6116a902379c7238";

/// The owner, at block time `now`.
fn owner_at(now: u64) {
    let mut context = context("owner.near");
    context.block_timestamp(now);
    testing_env!(context.build());
}

/// Armed as in the other tests (NEAR and token policies with cap 50 and
/// window 100, `bob.near` allowed, a mandate to `agent_key()` that never
/// expires), with an EVM configuration, a NEAR and an EVM treasury, and then
/// the timelock switched on. Set up in this order because, with the timelock
/// off, everything is instant; raising the delay is itself instant.
fn armed() -> Minder {
    let mut minder = armed_as_agent();
    owner_at(T0);
    minder.set_evm_config(
        account("signer.near"),
        "minder-evm-1".to_string(),
        test_sender_hex(),
        5,
        U128(5),
        U128(1),
        None,
        None,
    );
    minder.set_treasury(Some(account(TREASURY)), Some(EVM_TREASURY.to_string()));
    minder.set_timelock_delay(U64(DELAY));
    minder
}

fn panic_text<R>(call: impl FnOnce() -> R) -> String {
    let payload = catch_unwind(AssertUnwindSafe(call))
        .err()
        .expect("expected a panic");
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|text| text.to_string()))
        .unwrap_or_default()
}

fn refused_as_timelocked<R>(call: impl FnOnce() -> R) {
    let text = panic_text(call);
    assert!(
        text.contains("Timelocked"),
        "expected a timelock refusal: {text}"
    );
}

fn policy_of(minder: &Minder, asset: AssetId) -> (u128, u128, Option<u128>) {
    let policy = minder.get_asset_policy(asset).expect("a policy");
    (
        policy.per_tx_cap.0,
        policy.window_limit.0,
        policy.epoch_budget.map(|b| b.0),
    )
}

/// A loosening change goes through the whole mechanism: refused when called
/// directly, proposed, not executable before its time, executable at it, gone
/// after. `observe` reads the state the change affects.
fn through_the_delay<T: PartialEq + std::fmt::Debug>(
    minder: &mut Minder,
    direct: impl FnOnce(&mut Minder),
    change: OwnerChange,
    observe: impl Fn(&Minder) -> T,
    after: T,
) {
    let before = observe(minder);
    assert_ne!(before, after, "the change must change something");

    owner_at(T0);
    refused_as_timelocked(|| direct(minder));
    assert_eq!(observe(minder), before, "a refused change changes nothing");

    let id = minder.propose_change(change);
    let pending = minder.get_pending_change(id).expect("pending");
    assert_eq!(pending.ready_at.0, T0 + DELAY);
    assert_eq!(observe(minder), before, "proposing changes nothing");

    owner_at(T0 + DELAY - 1);
    let early = panic_text(|| {
        let _ = minder.execute_change(id);
    });
    assert!(early.contains("Not ready"), "{early}");
    assert_eq!(observe(minder), before);
    assert!(minder.get_pending_change(id).is_some(), "still pending");

    owner_at(T0 + DELAY);
    let _ = minder.execute_change(id);
    assert_eq!(
        observe(minder),
        after,
        "it takes effect once the delay has passed"
    );
    assert!(
        minder.get_pending_change(id).is_none(),
        "and is no longer pending"
    );
    // Back to the start for the next change.
    owner_at(T0);
}

// ---- Instant: tightening and emergency actions ----

#[test]
fn freeze_and_revoke_take_effect_at_once() {
    let mut minder = armed();
    owner_at(T0);
    minder.freeze();
    assert_eq!(minder.get_status(), Status::Frozen);
    minder.revoke_mandate();
    assert!(minder.get_mandate().unwrap().revoked);
}

#[test]
fn a_policy_change_that_is_tighter_or_equal_in_every_field_is_instant() {
    let mut minder = armed();
    owner_at(T0);
    let near = AssetId::Near;
    // Lower cap and window.
    minder.set_asset_policy(near.clone(), U128(40), U128(90), None);
    assert_eq!(policy_of(&minder, near.clone()), (40, 90, None));
    // A budget where there was none is a tightening.
    minder.set_asset_policy(near.clone(), U128(40), U128(90), Some(U128(500)));
    assert_eq!(policy_of(&minder, near.clone()), (40, 90, Some(500)));
    // Equal is not loosening, and neither is a lower budget.
    minder.set_asset_policy(near.clone(), U128(40), U128(90), Some(U128(500)));
    minder.set_asset_policy(near.clone(), U128(40), U128(90), Some(U128(400)));
    assert_eq!(policy_of(&minder, near), (40, 90, Some(400)));
}

#[test]
fn removing_an_asset_or_a_recipient_is_instant() {
    let mut minder = armed();
    owner_at(T0);
    minder.remove_asset_policy(AssetId::Near);
    assert!(minder.get_asset_policy(AssetId::Near).is_none());
    minder.set_recipient(account("bob.near"), false);
    assert!(!minder.is_recipient_allowed(account("bob.near")));
    // Removing what is not there changes nothing and is not loosening.
    minder.set_recipient(account("bob.near"), false);
}

#[test]
fn a_grant_that_does_not_extend_a_live_mandate_is_instant() {
    let mut minder = armed();
    owner_at(T0);
    // The armed mandate never expires, so any new expiry is not an extension.
    minder.grant_mandate(other_key(), U64(T0 + 5_000), None);
    assert_eq!(minder.get_mandate().unwrap().agent_key, other_key());
    // Once revoked, a grant restores authority at once, whatever its expiry:
    // the agent is bounded by the policy, which cannot be loosened instantly.
    minder.revoke_mandate();
    minder.grant_mandate(agent_key(), U64(T0 + 1_000_000_000), None);
    assert_eq!(minder.get_mandate().unwrap().epoch, 3);
}

#[test]
fn recovery_to_the_treasury_and_to_the_derived_address_is_instant() {
    let mut minder = armed();
    owner_at(T0);
    let _ = minder.owner_withdraw(AssetId::Near, U128(10), account(TREASURY));
    assert!(get_created_receipts()
        .iter()
        .any(|receipt| receipt.receiver_id == account(TREASURY)));
    let _ = minder.owner_withdraw(
        AssetId::Nep141(account("usdc.near")),
        U128(10),
        account(TREASURY),
    );

    let _ = minder.owner_evm_withdraw(EVM_TREASURY.to_string(), U128(1000), U64(0));
    let _ = minder.owner_evm_token_withdraw(
        TOKEN.to_string(),
        EVM_TREASURY.to_string(),
        U128(1000),
        U64(1),
    );
    // A self-transfer moves nothing out: the cancellation of a stuck transaction.
    let _ = minder.owner_evm_withdraw(test_sender_hex(), U128(0), U64(0));
    let signed = get_created_receipts()
        .iter()
        .filter(|receipt| receipt.receiver_id == account("signer.near"))
        .count();
    assert!(signed >= 1, "the signer was asked");
}

#[test]
fn raising_the_delay_is_instant_and_the_timelock_can_be_enabled_later() {
    let mut minder = armed_as_agent(); // the timelock is off
    owner_at(T0);
    minder.set_asset_policy(AssetId::Near, U128(500), U128(900), None); // instant, off
    minder.set_timelock_delay(U64(DELAY));
    refused_as_timelocked(|| minder.set_asset_policy(AssetId::Near, U128(600), U128(900), None));
    minder.set_timelock_delay(U64(2 * DELAY)); // raising stays instant
    assert_eq!(minder.get_config().timelock_delay, U64(2 * DELAY));
}

// ---- Delayed: loosening ----

#[test]
fn a_raised_cap_window_or_budget_is_delayed() {
    let mut minder = armed();
    through_the_delay(
        &mut minder,
        |m| m.set_asset_policy(AssetId::Near, U128(60), U128(100), None),
        OwnerChange::SetAssetPolicy {
            asset: AssetId::Near,
            per_tx_cap: U128(60),
            window_limit: U128(100),
            epoch_budget: None,
        },
        |m| policy_of(m, AssetId::Near),
        (60, 100, None),
    );
    through_the_delay(
        &mut minder,
        |m| m.set_asset_policy(AssetId::Near, U128(60), U128(120), None),
        OwnerChange::SetAssetPolicy {
            asset: AssetId::Near,
            per_tx_cap: U128(60),
            window_limit: U128(120),
            epoch_budget: None,
        },
        |m| policy_of(m, AssetId::Near),
        (60, 120, None),
    );
    // A budget is tightening when added; raising it, or dropping it, loosens.
    owner_at(T0);
    minder.set_asset_policy(AssetId::Near, U128(60), U128(120), Some(U128(300)));
    through_the_delay(
        &mut minder,
        |m| m.set_asset_policy(AssetId::Near, U128(60), U128(120), Some(U128(301))),
        OwnerChange::SetAssetPolicy {
            asset: AssetId::Near,
            per_tx_cap: U128(60),
            window_limit: U128(120),
            epoch_budget: Some(U128(301)),
        },
        |m| policy_of(m, AssetId::Near),
        (60, 120, Some(301)),
    );
    through_the_delay(
        &mut minder,
        |m| m.set_asset_policy(AssetId::Near, U128(60), U128(120), None),
        OwnerChange::SetAssetPolicy {
            asset: AssetId::Near,
            per_tx_cap: U128(60),
            window_limit: U128(120),
            epoch_budget: None,
        },
        |m| policy_of(m, AssetId::Near),
        (60, 120, None),
    );
}

#[test]
fn a_mixed_tightening_and_loosening_policy_change_is_loosening() {
    let mut minder = armed();
    owner_at(T0);
    // The cap goes down but the window goes up: one loosening field decides.
    refused_as_timelocked(|| minder.set_asset_policy(AssetId::Near, U128(10), U128(101), None));
    // The cap goes down and the budget is dropped from a policy that has one.
    minder.set_asset_policy(AssetId::Near, U128(50), U128(100), Some(U128(400)));
    refused_as_timelocked(|| minder.set_asset_policy(AssetId::Near, U128(10), U128(100), None));
    assert_eq!(policy_of(&minder, AssetId::Near), (50, 100, Some(400)));
    // The same mixed change goes through the delay as one change.
    let id = minder.propose_change(OwnerChange::SetAssetPolicy {
        asset: AssetId::Near,
        per_tx_cap: U128(10),
        window_limit: U128(101),
        epoch_budget: Some(U128(400)),
    });
    assert!(minder.get_pending_change(id).is_some());
}

#[test]
fn adding_an_asset_is_delayed() {
    let mut minder = armed();
    let dai = AssetId::Nep141(account("dai.near"));
    let key = dai.clone();
    through_the_delay(
        &mut minder,
        |m| m.set_asset_policy(dai, U128(5), U128(5), None),
        OwnerChange::SetAssetPolicy {
            asset: key.clone(),
            per_tx_cap: U128(5),
            window_limit: U128(5),
            epoch_budget: None,
        },
        |m| m.get_asset_policy(key.clone()).is_some(),
        true,
    );
}

#[test]
fn adding_a_recipient_is_delayed_on_both_surfaces() {
    let mut minder = armed();
    through_the_delay(
        &mut minder,
        |m| m.set_recipient(account("carol.near"), true),
        OwnerChange::SetRecipient {
            recipient: account("carol.near"),
            allowed: true,
        },
        |m| m.is_recipient_allowed(account("carol.near")),
        true,
    );
    through_the_delay(
        &mut minder,
        |m| m.set_evm_recipient(STRANGER.to_string(), true),
        OwnerChange::SetEvmRecipient {
            address: STRANGER.to_string(),
            allowed: true,
        },
        |m| m.is_evm_recipient_allowed(STRANGER.to_string()),
        true,
    );
}

#[test]
fn extending_a_live_mandate_is_delayed() {
    let mut minder = armed();
    owner_at(T0);
    minder.grant_mandate(other_key(), U64(T0 + 1_000), None); // short, live
    let extended = U64(T0 + 100_000);
    let key = agent_key();
    through_the_delay(
        &mut minder,
        |m| m.grant_mandate(key.clone(), extended, None),
        OwnerChange::GrantMandate {
            agent_key: agent_key(),
            expiry: extended,
            scope: None,
        },
        |m| m.get_mandate().unwrap().expiry.0,
        extended.0,
    );
}

#[test]
fn a_grant_whose_time_has_passed_by_execution_reverts_and_stays_pending() {
    let mut minder = armed();
    owner_at(T0);
    minder.grant_mandate(other_key(), U64(T0 + 100), None);
    let id = minder.propose_change(OwnerChange::GrantMandate {
        agent_key: agent_key(),
        expiry: U64(T0 + DELAY / 2 + 200),
        scope: Some(MandateScope::default()),
    });
    owner_at(T0 + DELAY + 1_000);
    let text = panic_text(|| {
        let _ = minder.execute_change(id);
    });
    assert!(text.contains("expiry must be in the future"), "{text}");
    assert!(
        minder.get_pending_change(id).is_some(),
        "it stays, to be cancelled"
    );
    minder.cancel_change(id);
}

#[test]
fn unfreezing_is_delayed_but_freezing_is_not() {
    let mut minder = armed();
    owner_at(T0);
    minder.freeze();
    through_the_delay(
        &mut minder,
        |m| m.unfreeze(),
        OwnerChange::Unfreeze,
        |m| m.get_status(),
        Status::Active,
    );
    // Unfreezing a contract that is not frozen changes nothing and is instant.
    minder.unfreeze();
}

#[test]
fn loosening_the_evm_configuration_is_delayed_and_tightening_it_is_not() {
    let mut minder = armed();
    owner_at(T0);
    let configure = |m: &mut Minder, fee: u128, gas: Option<u64>| {
        m.set_evm_config(
            account("signer.near"),
            "minder-evm-1".to_string(),
            test_sender_hex(),
            5,
            U128(fee),
            U128(1),
            gas,
            Some(100_000),
        )
    };
    configure(&mut minder, 4, None); // a lower fee: tighter
    assert_eq!(minder.get_evm_config().unwrap().max_fee_per_gas, U128(4));
    refused_as_timelocked(|| configure(&mut minder, 6, None));
    refused_as_timelocked(|| configure(&mut minder, 4, Some(30_000)));
    through_the_delay(
        &mut minder,
        |m| configure(m, 9, None),
        OwnerChange::SetEvmConfig {
            signer_id: account("signer.near"),
            derivation_path: "minder-evm-1".to_string(),
            sender: test_sender_hex(),
            chain_id: 5,
            max_fee_per_gas: U128(9),
            max_priority_fee_per_gas: U128(1),
            gas_limit: None,
            token_gas_limit: Some(100_000),
        },
        |m| m.get_evm_config().unwrap().max_fee_per_gas.0,
        9,
    );
}

#[test]
fn lowering_the_delay_or_changing_the_treasury_is_delayed() {
    let mut minder = armed();
    through_the_delay(
        &mut minder,
        |m| m.set_timelock_delay(U64(DELAY / 2)),
        OwnerChange::SetTimelockDelay {
            delay: U64(DELAY / 2),
        },
        |m| m.get_config().timelock_delay.0,
        DELAY / 2,
    );
}

#[test]
fn changing_the_treasury_is_delayed() {
    let mut minder = armed();
    through_the_delay(
        &mut minder,
        |m| {
            m.set_treasury(
                Some(account("elsewhere.near")),
                Some(EVM_TREASURY.to_string()),
            )
        },
        OwnerChange::SetTreasury {
            near: Some(account("elsewhere.near")),
            evm: Some(EVM_TREASURY.to_string()),
        },
        |m| m.get_config().near_treasury,
        Some(account("elsewhere.near")),
    );
    owner_at(T0);
    through_the_delay(
        &mut minder,
        |m| m.set_treasury(Some(account("elsewhere.near")), Some(STRANGER.to_string())),
        OwnerChange::SetTreasury {
            near: Some(account("elsewhere.near")),
            evm: Some(STRANGER.to_string()),
        },
        |m| m.get_config().evm_treasury,
        Some(STRANGER.to_string()),
    );
    // Setting it to what it already is changes nothing and is instant.
    owner_at(T0);
    minder.set_treasury(Some(account("elsewhere.near")), Some(STRANGER.to_string()));
}

#[test]
fn recovery_to_any_other_destination_is_delayed() {
    let mut minder = armed();
    owner_at(T0);
    refused_as_timelocked(|| {
        let _ = minder.owner_withdraw(AssetId::Near, U128(10), account("mallory.near"));
    });
    refused_as_timelocked(|| {
        let _ = minder.owner_evm_withdraw(ATTACKER_EVM.to_string(), U128(1), U64(0));
    });
    refused_as_timelocked(|| {
        let _ = minder.owner_evm_token_withdraw(
            TOKEN.to_string(),
            ATTACKER_EVM.to_string(),
            U128(1),
            U64(0),
        );
    });
    assert!(
        get_created_receipts().is_empty(),
        "nothing was sent or signed"
    );

    // Proposed, held, and executed after the delay: the transfer is made then.
    let id = minder.propose_change(OwnerChange::OwnerWithdraw {
        asset: AssetId::Near,
        amount: U128(10),
        to: account("mallory.near"),
    });
    owner_at(T0 + DELAY - 1);
    assert!(panic_text(|| {
        let _ = minder.execute_change(id);
    })
    .contains("Not ready"));
    assert!(get_created_receipts().is_empty());
    owner_at(T0 + DELAY);
    let _ = minder.execute_change(id);
    assert!(get_created_receipts()
        .iter()
        .any(|receipt| receipt.receiver_id == account("mallory.near")));

    // The EVM recoveries, the same way: the signer is asked only at execution.
    owner_at(T0);
    let native = minder.propose_change(OwnerChange::OwnerEvmWithdraw {
        recipient: ATTACKER_EVM.to_string(),
        value: U128(1),
        nonce: U64(3),
    });
    let token = minder.propose_change(OwnerChange::OwnerEvmTokenWithdraw {
        token: TOKEN.to_string(),
        recipient: ATTACKER_EVM.to_string(),
        amount: U128(1),
        nonce: U64(4),
    });
    assert_eq!(
        minder.get_evm_nonce(5),
        U64(0),
        "the nonce is not touched by a proposal"
    );
    owner_at(T0 + DELAY);
    let _ = minder.execute_change(native);
    assert_eq!(minder.get_evm_nonce(5), U64(4));
    let _ = minder.execute_change(token);
    assert_eq!(minder.get_evm_nonce(5), U64(5));
    assert!(get_created_receipts()
        .iter()
        .any(|receipt| receipt.receiver_id == account("signer.near")));
}

// ---- The mechanism ----

#[test]
fn cancel_removes_a_pending_change_and_it_can_no_longer_be_executed() {
    let mut minder = armed();
    owner_at(T0);
    let id = minder.propose_change(OwnerChange::SetRecipient {
        recipient: account("carol.near"),
        allowed: true,
    });
    minder.cancel_change(id);
    assert!(minder.get_pending_change(id).is_none());
    owner_at(T0 + DELAY);
    assert!(panic_text(|| {
        let _ = minder.execute_change(id);
    })
    .contains("No such pending change"));
    assert!(!minder.is_recipient_allowed(account("carol.near")));
    assert!(panic_text(|| minder.cancel_change(id)).contains("No such pending change"));
}

#[test]
fn ids_are_unique_and_pending_changes_are_listed_in_id_order() {
    let mut minder = armed();
    owner_at(T0);
    let ids: Vec<U64> = ["carol.near", "dave.near", "erin.near"]
        .into_iter()
        .map(|name| {
            minder.propose_change(OwnerChange::SetRecipient {
                recipient: account(name),
                allowed: true,
            })
        })
        .collect();
    assert_eq!(ids, [U64(1), U64(2), U64(3)]);
    minder.cancel_change(U64(2));
    let again = minder.propose_change(OwnerChange::SetRecipient {
        recipient: account("frank.near"),
        allowed: true,
    });
    assert_eq!(again, U64(4), "an id is never reused");
    let listed: Vec<u64> = minder
        .get_pending_changes(None, None)
        .iter()
        .map(|pending| pending.id.0)
        .collect();
    assert_eq!(listed, [1, 3, 4]);
}

#[test]
fn only_a_loosening_change_can_be_proposed_and_only_a_valid_one() {
    let mut minder = armed();
    owner_at(T0);
    for change in [
        // Tightening, equal, or already in effect: call the method directly.
        OwnerChange::SetAssetPolicy {
            asset: AssetId::Near,
            per_tx_cap: U128(1),
            window_limit: U128(1),
            epoch_budget: None,
        },
        OwnerChange::SetRecipient {
            recipient: account("bob.near"),
            allowed: true,
        },
        OwnerChange::Unfreeze,
        OwnerChange::OwnerWithdraw {
            asset: AssetId::Near,
            amount: U128(1),
            to: account(TREASURY),
        },
    ] {
        let text = panic_text(|| minder.propose_change(change.clone()));
        assert!(text.contains("Not timelocked"), "{change:?}: {text}");
    }
    // Malformed changes are refused when proposed, not when due.
    for change in [
        OwnerChange::SetEvmRecipient {
            address: "0x1234".to_string(),
            allowed: true,
        },
        OwnerChange::OwnerEvmWithdraw {
            recipient: ATTACKER_EVM.to_string(),
            value: U128(1),
            nonce: U64(u64::MAX),
        },
        OwnerChange::OwnerWithdraw {
            asset: AssetId::Evm(5),
            amount: U128(1),
            to: account("mallory.near"),
        },
        OwnerChange::SetTreasury {
            near: None,
            evm: Some("0x0000000000000000000000000000000000000000".to_string()),
        },
        OwnerChange::GrantMandate {
            agent_key: agent_key(),
            expiry: U64(1),
            scope: None,
        },
    ] {
        assert!(
            catch_unwind(AssertUnwindSafe(|| minder.propose_change(change.clone()))).is_err(),
            "{change:?}"
        );
    }
    assert!(minder.get_pending_changes(None, None).is_empty());
}

#[test]
fn only_the_owner_can_propose_execute_or_cancel() {
    let mut minder = armed();
    owner_at(T0);
    let id = minder.propose_change(OwnerChange::SetRecipient {
        recipient: account("carol.near"),
        allowed: true,
    });
    for who in ["mallory.near", "minder.near"] {
        testing_env!(context(who).build());
        assert!(panic_text(|| {
            minder.propose_change(OwnerChange::Unfreeze);
        })
        .contains("Only the owner"));
        assert!(panic_text(|| {
            let _ = minder.execute_change(id);
        })
        .contains("Only the owner"));
        assert!(panic_text(|| minder.cancel_change(id)).contains("Only the owner"));
        assert!(panic_text(|| minder.set_timelock_delay(U64(1))).contains("Only the owner"));
        assert!(panic_text(|| minder.set_treasury(None, None)).contains("Only the owner"));
    }
    testing_env!(agent_context().build());
    assert!(panic_text(|| minder.cancel_change(id)).contains("Only the owner"));
}

#[test]
fn the_number_of_pending_changes_is_bounded() {
    let mut minder = armed();
    owner_at(T0);
    for i in 0..32 {
        minder.propose_change(OwnerChange::SetRecipient {
            recipient: account(&format!("r{i}.near")),
            allowed: true,
        });
    }
    let text = panic_text(|| {
        minder.propose_change(OwnerChange::SetRecipient {
            recipient: account("one-more.near"),
            allowed: true,
        });
    });
    assert!(text.contains("Too many pending changes"), "{text}");
}

#[test]
fn proposals_executions_and_cancellations_are_logged() {
    let mut minder = armed();
    owner_at(T0);
    let kept = minder.propose_change(OwnerChange::SetRecipient {
        recipient: account("carol.near"),
        allowed: true,
    });
    let dropped = minder.propose_change(OwnerChange::SetRecipient {
        recipient: account("dave.near"),
        allowed: true,
    });
    let logs = get_logs();
    let events: Vec<_> = logs
        .iter()
        .filter_map(|line| line.strip_prefix("EVENT_JSON:"))
        .map(|json| near_sdk::serde_json::from_str::<near_sdk::serde_json::Value>(json).unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["standard"], "minder");
    assert_eq!(events[0]["event"], "owner_change_proposed");
    assert_eq!(events[0]["data"][0]["id"], "1");
    assert_eq!(events[0]["data"][0]["ready_at"], (T0 + DELAY).to_string());

    minder.cancel_change(dropped);
    assert!(get_logs()
        .iter()
        .any(|l| l.contains("owner_change_cancelled")));
    owner_at(T0 + DELAY);
    let _ = minder.execute_change(kept);
    assert!(get_logs()
        .iter()
        .any(|l| l.contains("owner_change_executed")));
}

#[test]
fn with_the_timelock_off_every_change_is_instant_and_nothing_can_be_proposed() {
    let mut minder = armed_as_agent();
    owner_at(T0);
    minder.set_asset_policy(AssetId::Near, U128(5000), U128(9000), None);
    minder.set_recipient(account("carol.near"), true);
    let _ = minder.owner_withdraw(AssetId::Near, U128(1), account("anyone.near"));
    let text = panic_text(|| {
        minder.propose_change(OwnerChange::SetRecipient {
            recipient: account("dave.near"),
            allowed: true,
        });
    });
    assert!(text.contains("Not timelocked"), "{text}");
}

// ---- The compromised owner key ----

#[test]
fn a_compromised_owner_key_cannot_widen_authority_or_redirect_funds_at_once() {
    let mut minder = armed();
    owner_at(T0);
    let policy_before = policy_of(&minder, AssetId::Near);
    let config_before = minder.get_config();

    // The attacker holds the owner key and tries, in turn, to ...
    // ... raise a cap,
    refused_as_timelocked(|| {
        minder.set_asset_policy(AssetId::Near, U128(u128::MAX), U128(u128::MAX), None)
    });
    // ... let the agent pay the attacker,
    refused_as_timelocked(|| minder.set_recipient(account("attacker.near"), true));
    refused_as_timelocked(|| minder.set_evm_recipient(ATTACKER_EVM.to_string(), true));
    // ... move the treasury to the attacker,
    refused_as_timelocked(|| {
        minder.set_treasury(
            Some(account("attacker.near")),
            Some(ATTACKER_EVM.to_string()),
        )
    });
    // ... drop the delay,
    refused_as_timelocked(|| minder.set_timelock_delay(U64(0)));
    // ... and withdraw to their own address, on every surface.
    refused_as_timelocked(|| {
        let _ = minder.owner_withdraw(AssetId::Near, U128(1), account("attacker.near"));
    });
    refused_as_timelocked(|| {
        let _ = minder.owner_evm_withdraw(ATTACKER_EVM.to_string(), U128(1), U64(0));
    });
    refused_as_timelocked(|| {
        let _ = minder.owner_evm_token_withdraw(
            TOKEN.to_string(),
            ATTACKER_EVM.to_string(),
            U128(1),
            U64(0),
        );
    });
    assert_eq!(policy_of(&minder, AssetId::Near), policy_before);
    assert_eq!(minder.get_config(), config_before);
    assert!(
        get_created_receipts().is_empty(),
        "no value moved, nothing signed"
    );

    // What they can do is queue those changes, in view of everyone.
    let queued = minder.propose_change(OwnerChange::SetRecipient {
        recipient: account("attacker.near"),
        allowed: true,
    });
    assert!(get_logs()
        .iter()
        .any(|line| line.contains("owner_change_proposed")));

    // Meanwhile a legitimate freeze (and a cap-lowering) stays instant, and the
    // queued change is cancelled before it is due: it never takes effect.
    minder.freeze();
    assert_eq!(minder.get_status(), Status::Frozen);
    minder.set_asset_policy(AssetId::Near, U128(1), U128(1), None);
    minder.cancel_change(queued);
    owner_at(T0 + 10 * DELAY);
    assert!(panic_text(|| {
        let _ = minder.execute_change(queued);
    })
    .contains("No such pending change"));
    assert!(!minder.is_recipient_allowed(account("attacker.near")));
}

#[test]
fn tightening_works_while_a_loosening_of_the_same_thing_is_pending() {
    let mut minder = armed();
    owner_at(T0);
    let raise = minder.propose_change(OwnerChange::SetAssetPolicy {
        asset: AssetId::Near,
        per_tx_cap: U128(500),
        window_limit: U128(1000),
        epoch_budget: None,
    });
    // An emergency lowering is not blocked by the pending raise ...
    minder.set_asset_policy(AssetId::Near, U128(5), U128(10), None);
    minder.freeze();
    minder.revoke_mandate();
    assert_eq!(policy_of(&minder, AssetId::Near), (5, 10, None));
    // ... and the raise is still pending, for the owner to cancel.
    assert!(minder.get_pending_change(raise).is_some());
    minder.cancel_change(raise);
}
