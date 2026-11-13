//! Unit tests for the guardian: a second account whose powers are exactly the
//! stop-powers (`freeze`, `revoke_mandate`, and cancelling a pending change
//! other than one to the guardian itself), and nothing that moves or widens
//! authority.

use std::panic::{catch_unwind, AssertUnwindSafe};

use near_sdk::json_types::{U128, U64};
use near_sdk::test_utils::{get_created_receipts, get_logs};
use near_sdk::testing_env;

use crate::test_support::*;
use crate::{AssetId, Minder, OwnerChange, Status};

const DELAY: u64 = 10_000;
const T0: u64 = 1_000_000;
const GUARDIAN: &str = "guardian.near";
const OTHER_GUARDIAN: &str = "guardian2.near";
const TREASURY: &str = "treasury.near";
const EVM_TREASURY: &str = "0x00000000000000000000000000000000000000aa";
const STRANGER: &str = "0x00000000000000000000000000000000000000bb";
const TOKEN: &str = "0x1c7d4b196cb0c7b01d743fbc6116a902379c7238";

fn at(caller: &str, now: u64) {
    let mut context = context(caller);
    context.block_timestamp(now);
    testing_env!(context.build());
}

fn owner_at(now: u64) {
    at("owner.near", now);
}

fn guardian_at(now: u64) {
    at(GUARDIAN, now);
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

/// Armed (policies, a recipient, a live mandate, an EVM configuration, both
/// treasuries), then the guardian added and the timelock switched on. Adding a
/// guardian is instant, so the order matters only for the delay.
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
    minder.set_guardian(Some(account(GUARDIAN)));
    minder.set_timelock_delay(U64(DELAY));
    minder
}

fn propose_recipient(minder: &mut Minder, name: &str) -> U64 {
    owner_at(T0);
    minder.propose_change(OwnerChange::SetRecipient {
        recipient: account(name),
        allowed: true,
    })
}

// ---- The guardian's three powers ----

#[test]
fn the_guardian_can_freeze() {
    let mut minder = armed();
    guardian_at(T0);
    minder.freeze();
    assert_eq!(minder.get_status(), Status::Frozen);
}

#[test]
fn the_guardian_can_revoke_the_mandate() {
    let mut minder = armed();
    guardian_at(T0);
    minder.revoke_mandate();
    assert!(minder.get_mandate().expect("a mandate").revoked);
    // The key is deleted by the contract, as for the owner.
    assert!(!get_created_receipts().is_empty());
}

#[test]
fn the_guardian_can_cancel_a_pending_change_that_is_not_about_the_guardian() {
    let mut minder = armed();
    let id = propose_recipient(&mut minder, "carol.near");
    guardian_at(T0);
    minder.cancel_change(id);
    assert!(minder.get_pending_change(id).is_none());
    let logs = get_logs().join("\n");
    assert!(logs.contains("owner_change_cancelled"), "{logs}");
    // The event names who cancelled.
    assert!(logs.contains(GUARDIAN), "{logs}");
}

// ---- Nothing else ----

/// The state a stranger to the guardian's powers must leave alone.
fn snapshot(minder: &Minder) -> String {
    format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}",
        minder.get_config(),
        minder.get_mandate(),
        minder.get_asset_policies(None, None),
        minder.get_recipients(None, None),
        minder.get_pending_changes(None, None),
    )
}

/// Every other method of the owner, called by the guardian, is refused as
/// owner-only, changes nothing and creates no receipt (moves no funds).
#[test]
fn every_other_owner_method_is_refused_for_the_guardian() {
    let mut minder = armed();
    // Something is pending, and the contract is frozen, so that `unfreeze`,
    // `execute_change` and the rest have something real to act on.
    let id = propose_recipient(&mut minder, "carol.near");
    owner_at(T0);
    minder.freeze();
    owner_at(T0 + DELAY);
    let before = snapshot(&minder);
    guardian_at(T0 + DELAY);

    let refusals: Vec<(&str, String)> = vec![
        (
            "set_asset_policy",
            panic_text(|| {
                minder.set_asset_policy(AssetId::Near, U128(1), U128(1), None);
            }),
        ),
        (
            "remove_asset_policy",
            panic_text(|| minder.remove_asset_policy(AssetId::Near)),
        ),
        (
            "set_recipient",
            panic_text(|| minder.set_recipient(account("eve.near"), true)),
        ),
        (
            "set_evm_recipient",
            panic_text(|| minder.set_evm_recipient(STRANGER.to_string(), true)),
        ),
        (
            "grant_mandate",
            panic_text(|| minder.grant_mandate(other_key(), U64(u64::MAX), None)),
        ),
        ("unfreeze", panic_text(|| minder.unfreeze())),
        (
            "owner_withdraw",
            panic_text(|| {
                let _ = minder.owner_withdraw(AssetId::Near, U128(1), account(TREASURY));
            }),
        ),
        (
            "owner_evm_withdraw",
            panic_text(|| {
                let _ = minder.owner_evm_withdraw(EVM_TREASURY.to_string(), U128(1), U64(0));
            }),
        ),
        (
            "owner_evm_token_withdraw",
            panic_text(|| {
                let _ = minder.owner_evm_token_withdraw(
                    TOKEN.to_string(),
                    EVM_TREASURY.to_string(),
                    U128(1),
                    U64(0),
                );
            }),
        ),
        (
            "set_evm_config",
            panic_text(|| {
                minder.set_evm_config(
                    account("signer.near"),
                    "minder-evm-1".to_string(),
                    test_sender_hex(),
                    5,
                    U128(1),
                    U128(1),
                    None,
                    None,
                )
            }),
        ),
        (
            "raise_evm_nonce",
            panic_text(|| minder.raise_evm_nonce(5, U64(9))),
        ),
        (
            "propose_change",
            panic_text(|| {
                minder.propose_change(OwnerChange::SetRecipient {
                    recipient: account("eve.near"),
                    allowed: true,
                });
            }),
        ),
        (
            "execute_change",
            panic_text(|| {
                let _ = minder.execute_change(id);
            }),
        ),
        (
            "set_timelock_delay",
            panic_text(|| minder.set_timelock_delay(U64(0))),
        ),
        (
            "set_treasury",
            panic_text(|| minder.set_treasury(Some(account("eve.near")), None)),
        ),
        (
            "set_guardian",
            panic_text(|| minder.set_guardian(Some(account(OTHER_GUARDIAN)))),
        ),
    ];
    for (method, text) in &refusals {
        assert!(
            text.contains("Only the owner can call this method"),
            "{method}: {text}"
        );
    }
    assert_eq!(snapshot(&minder), before, "a refused call changed state");
    assert!(
        get_created_receipts().is_empty(),
        "a refused call moved funds or made a promise"
    );
}

/// A compromised guardian key, start to finish: it can stop the contract and
/// cancel what the owner queued, and it cannot move funds, loosen, grant,
/// reconfigure, or touch the treasury or the delay.
#[test]
fn a_compromised_guardian_can_only_stop_never_move_or_widen() {
    let mut minder = armed();
    let id = propose_recipient(&mut minder, "carol.near");
    guardian_at(T0);

    // The stop-powers work ...
    minder.freeze();
    minder.revoke_mandate();
    minder.cancel_change(id);
    assert_eq!(minder.get_status(), Status::Frozen);
    assert!(minder.get_mandate().expect("a mandate").revoked);
    assert!(minder.get_pending_change(id).is_none());

    // ... and nothing that loosens does.
    let before = snapshot(&minder);
    for attempt in [
        panic_text(|| {
            minder.set_asset_policy(AssetId::Near, U128(9_999), U128(9_999), None);
        }),
        panic_text(|| minder.set_recipient(account("eve.near"), true)),
        panic_text(|| minder.grant_mandate(other_key(), U64(u64::MAX), None)),
        panic_text(|| minder.unfreeze()),
        panic_text(|| {
            let _ = minder.owner_withdraw(AssetId::Near, U128(1), account("eve.near"));
        }),
        panic_text(|| minder.set_treasury(Some(account("eve.near")), None)),
        panic_text(|| minder.set_timelock_delay(U64(0))),
        panic_text(|| minder.set_guardian(None)),
    ] {
        assert!(attempt.contains("Only the owner"), "{attempt}");
    }
    assert_eq!(snapshot(&minder), before);
    assert!(get_created_receipts().len() <= 1, "only the key deletion");
}

#[test]
fn a_stranger_still_cannot_use_the_three_shared_methods() {
    let mut minder = armed();
    let id = propose_recipient(&mut minder, "carol.near");
    at("stranger.near", T0);
    for text in [
        panic_text(|| minder.freeze()),
        panic_text(|| minder.revoke_mandate()),
        panic_text(|| minder.cancel_change(id)),
    ] {
        assert!(text.contains("Only the owner or the guardian"), "{text}");
    }
}

#[test]
fn with_no_guardian_the_shared_methods_are_the_owners_alone() {
    let mut minder = armed_as_agent();
    guardian_at(T0);
    let text = panic_text(|| minder.freeze());
    assert!(text.contains("Only the owner or the guardian"), "{text}");
}

// ---- The guardian cannot protect itself from removal ----

#[test]
fn the_guardian_cannot_cancel_its_own_removal_or_replacement_but_the_owner_can() {
    for replacement in [None, Some(account(OTHER_GUARDIAN))] {
        let mut minder = armed();
        owner_at(T0);
        let id = minder.propose_change(OwnerChange::SetGuardian {
            guardian: replacement.clone(),
        });
        guardian_at(T0);
        let text = panic_text(|| minder.cancel_change(id));
        assert!(
            text.contains("The guardian cannot cancel a change to the guardian"),
            "{text}"
        );
        assert!(minder.get_pending_change(id).is_some());

        // The owner can, and so can the delay run out.
        owner_at(T0);
        minder.cancel_change(id);
        assert!(minder.get_pending_change(id).is_none());
    }
}

#[test]
fn a_guardian_removal_executes_after_the_delay_whatever_the_guardian_does() {
    let mut minder = armed();
    owner_at(T0);
    let id = minder.propose_change(OwnerChange::SetGuardian { guardian: None });
    // The guardian does everything it can.
    guardian_at(T0);
    minder.freeze();
    minder.revoke_mandate();
    assert!(panic_text(|| minder.cancel_change(id)).contains("cannot cancel"));
    owner_at(T0 + DELAY - 1);
    assert!(panic_text(|| {
        let _ = minder.execute_change(id);
    })
    .contains("Not ready"));
    owner_at(T0 + DELAY);
    let _ = minder.execute_change(id);
    assert_eq!(minder.get_config().guardian, None);
    // With no guardian left, its powers are gone.
    guardian_at(T0 + DELAY);
    assert!(panic_text(|| minder.freeze()).contains("Only the owner or the guardian"));
}

// ---- Governance: adding is instant, removing and replacing are timelocked ----

#[test]
fn adding_a_guardian_is_instant_even_under_the_timelock() {
    let mut minder = armed_as_agent();
    owner_at(T0);
    minder.set_timelock_delay(U64(DELAY));
    minder.set_guardian(Some(account(GUARDIAN)));
    assert_eq!(minder.get_config().guardian, Some(account(GUARDIAN)));
}

#[test]
fn removing_or_replacing_a_guardian_is_timelocked() {
    for change in [None, Some(account(OTHER_GUARDIAN))] {
        let mut minder = armed();
        owner_at(T0);
        let text = panic_text(|| minder.set_guardian(change.clone()));
        assert!(text.contains("Timelocked"), "{text}");
        assert_eq!(minder.get_config().guardian, Some(account(GUARDIAN)));

        let id = minder.propose_change(OwnerChange::SetGuardian {
            guardian: change.clone(),
        });
        owner_at(T0 + DELAY - 1);
        assert!(panic_text(|| {
            let _ = minder.execute_change(id);
        })
        .contains("Not ready"));
        owner_at(T0 + DELAY);
        let _ = minder.execute_change(id);
        assert_eq!(minder.get_config().guardian, change);
    }
}

#[test]
fn setting_the_same_guardian_again_is_instant_and_changes_nothing() {
    let mut minder = armed();
    owner_at(T0);
    minder.set_guardian(Some(account(GUARDIAN)));
    assert_eq!(minder.get_config().guardian, Some(account(GUARDIAN)));
    // And it is not proposable: nothing loosens.
    let text = panic_text(|| {
        minder.propose_change(OwnerChange::SetGuardian {
            guardian: Some(account(GUARDIAN)),
        });
    });
    assert!(text.contains("Not timelocked"), "{text}");
}

#[test]
fn with_the_timelock_off_removing_the_guardian_is_instant() {
    let mut minder = armed_as_agent();
    owner_at(T0);
    minder.set_guardian(Some(account(GUARDIAN)));
    minder.set_guardian(None);
    assert_eq!(minder.get_config().guardian, None);
}

#[test]
fn the_guardian_can_be_neither_the_owner_nor_the_contract() {
    let mut minder = armed_as_agent();
    owner_at(T0);
    for name in ["owner.near", "minder.near"] {
        let text = panic_text(|| minder.set_guardian(Some(account(name))));
        assert!(text.contains("must be a different account"), "{text}");
        let text = panic_text(|| {
            minder.propose_change(OwnerChange::SetGuardian {
                guardian: Some(account(name)),
            });
        });
        assert!(text.contains("must be a different account"), "{text}");
    }
    for name in ["owner.near", "minder.near"] {
        testing_env!(context("owner.near").build());
        let text = panic_text(|| {
            Minder::init(
                account("owner.near"),
                U64(400),
                4,
                None,
                None,
                None,
                Some(account(name)),
            )
        });
        assert!(text.contains("must be a different account"), "{text}");
    }
}

#[test]
fn init_takes_an_optional_guardian() {
    testing_env!(context("owner.near").build());
    let with = Minder::init(
        account("owner.near"),
        U64(400),
        4,
        None,
        None,
        None,
        Some(account(GUARDIAN)),
    );
    assert_eq!(with.get_config().guardian, Some(account(GUARDIAN)));
    let without = Minder::init(account("owner.near"), U64(400), 4, None, None, None, None);
    assert_eq!(without.get_config().guardian, None);
}

// ---- The owner keeps everything ----

#[test]
fn the_owner_still_has_the_three_shared_methods_and_the_rest() {
    let mut minder = armed();
    let id = propose_recipient(&mut minder, "carol.near");
    owner_at(T0);
    minder.cancel_change(id);
    minder.freeze();
    minder.revoke_mandate();
    assert_eq!(minder.get_status(), Status::Frozen);
    assert!(minder.get_mandate().expect("a mandate").revoked);
    // Recovery to the treasury still works, frozen and revoked.
    let _ = minder.owner_withdraw(AssetId::Near, U128(1), account(TREASURY));
    // The owner can cancel a guardian change.
    let change = minder.propose_change(OwnerChange::SetGuardian { guardian: None });
    minder.cancel_change(change);
}

// ---- Stalemate safety ----

/// Owner and guardian both hostile, or fighting: neither can drain instantly,
/// and funds can always go to the treasury, so a standoff cannot lose them.
#[test]
fn a_standoff_between_owner_and_guardian_cannot_lose_funds() {
    let mut minder = armed();

    // The guardian freezes and revokes, and cancels whatever the owner queues.
    guardian_at(T0);
    minder.freeze();
    minder.revoke_mandate();

    // The owner (hostile, or only honest and blocked) cannot drain anywhere
    // but the treasury at once ...
    owner_at(T0);
    let text = panic_text(|| {
        let _ = minder.owner_withdraw(AssetId::Near, U128(1), account("eve.near"));
    });
    assert!(text.contains("Timelocked"), "{text}");
    let text = panic_text(|| {
        let _ = minder.owner_evm_withdraw(STRANGER.to_string(), U128(1), U64(0));
    });
    assert!(text.contains("Timelocked"), "{text}");
    let text = panic_text(|| minder.set_treasury(Some(account("eve.near")), None));
    assert!(text.contains("Timelocked"), "{text}");
    let text = panic_text(|| minder.unfreeze());
    assert!(text.contains("Timelocked"), "{text}");

    // ... a proposed drain is cancellable by the guardian ...
    let drain = minder.propose_change(OwnerChange::OwnerWithdraw {
        asset: AssetId::Near,
        amount: U128(1),
        to: account("eve.near"),
    });
    guardian_at(T0 + 1);
    minder.cancel_change(drain);
    assert!(minder.get_pending_change(drain).is_none());

    // ... while recovery to the treasury is instant, frozen, revoked, and
    // whatever the guardian does.
    owner_at(T0 + 2);
    let _ = minder.owner_withdraw(AssetId::Near, U128(1), account(TREASURY));
    let _ = minder.owner_evm_withdraw(EVM_TREASURY.to_string(), U128(1), U64(0));
}
