//! Unit tests for the EVM signing gate under the mocked NEAR environment:
//! what `request_evm_payment` reserves and sends to the signer, and how
//! `on_sign_complete` settles every possible signer result.

use std::panic::{catch_unwind, AssertUnwindSafe};

use near_sdk::json_types::{U128, U64};
use near_sdk::serde_json::{self, json, Value};
use near_sdk::test_utils::{get_created_receipts, get_logs};
use near_sdk::{testing_env, Gas, NearToken, PromiseResult, RuntimeFeesConfig};

use crate::evm::{self, Eip1559Tx};
use crate::test_support::*;
use crate::{AssetId, Decision, DenyReason, Minder};

const CHAIN_ID: u64 = 11_155_111;
const MAX_FEE: u128 = 2_000_000_000;
const MAX_PRIORITY_FEE: u128 = 1_000_000_000;
const GAS_LIMIT: u64 = 21_000;
/// What the fees of one payment are accounted at: 21000 * 2 gwei.
const FEE_COST: u128 = 42_000_000_000_000;
const EVM_PER_TX_CAP: u128 = 100_000_000_000_000;
const EVM_WINDOW_LIMIT: u128 = 150_000_000_000_000;
const RECIPIENT: &str = "0x70997970c51812dc3a010c7d01b50e0d17dc79c8";

const R: &str = "5b2fc0e1f6d4a7c98e3b1d2a4f5c6e7d8a9b0c1d2e3f405162738495a6b7c8d9";
const S: &str = "2a4d6f8091b3d5f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f";

fn evm_asset() -> AssetId {
    AssetId::Evm(CHAIN_ID)
}

fn recipient_bytes() -> [u8; 20] {
    evm::parse_address(RECIPIENT).unwrap()
}

/// Armed for NEAR as in the other unit tests, plus an EVM policy, an allowed
/// EVM recipient and an EVM config. The environment is left as the agent.
fn armed_evm() -> Minder {
    let mut minder = armed_as_agent();
    testing_env!(context("owner.near").build());
    minder.set_asset_policy(
        evm_asset(),
        U128(EVM_PER_TX_CAP),
        U128(EVM_WINDOW_LIMIT),
        None,
    );
    minder.set_evm_recipient(RECIPIENT.to_string(), true);
    minder.set_evm_config(
        account("signer.near"),
        "minder-evm-1".to_string(),
        test_sender_hex(),
        CHAIN_ID,
        U128(MAX_FEE),
        U128(MAX_PRIORITY_FEE),
        None,
    );
    testing_env!(agent_context().build());
    minder
}

fn request(minder: &mut Minder, amount: u128) {
    let _ = minder.request_evm_payment(RECIPIENT.to_string(), U128(amount));
}

fn expected_tx(nonce: u64, value: u128) -> Eip1559Tx {
    Eip1559Tx {
        chain_id: CHAIN_ID,
        nonce,
        max_priority_fee_per_gas: MAX_PRIORITY_FEE,
        max_fee_per_gas: MAX_FEE,
        gas_limit: GAS_LIMIT,
        to: recipient_bytes(),
        value,
    }
}

fn signature(affine_point: &str, scalar: &str, recovery_id: u8) -> Vec<u8> {
    json!({
        "scheme": "Secp256k1",
        "big_r": { "affine_point": affine_point },
        "s": { "scalar": scalar },
        "recovery_id": recovery_id,
    })
    .to_string()
    .into_bytes()
}

/// The fixture signature over the signing hash of `expected_tx(nonce, value)`.
fn fixture_for(nonce: u64, value: u128, signer: Signer) -> FixtureSignature {
    fixture_signature(&evm::signing_hash(&expected_tx(nonce, value)), signer)
}

/// A real signature by the configured sender over the first payment (nonce 0,
/// value 1000), as the signer contract would return it.
fn valid_signature() -> Vec<u8> {
    signer_response(&fixture_for(0, 1000, Signer::Sender))
}

/// Runs the settlement callback as the contract with the signer's result,
/// then returns the environment to the agent. Returns the logs the callback
/// produced, which resetting the environment would otherwise discard.
fn settle(
    minder: &mut Minder,
    outlay: u128,
    nonce: u64,
    value: u128,
    result: PromiseResult,
) -> Vec<String> {
    testing_env!(
        context("minder.near").build(),
        near_sdk::test_vm_config(),
        RuntimeFeesConfig::test(),
        Default::default(),
        vec![result]
    );
    minder.on_sign_complete(evm_asset(), outlay, expected_tx(nonce, value));
    let logs = get_logs();
    testing_env!(agent_context().build());
    logs
}

/// The mocked environment keeps storage across `testing_env!` calls within a
/// thread. Tests that loop over cases clear it so each case starts empty.
fn clear_storage() {
    let _ = near_sdk::mock::with_mocked_blockchain(|blockchain| blockchain.take_storage());
}

fn event_lines(logs: &[String]) -> Vec<Value> {
    logs.iter()
        .filter_map(|line| line.strip_prefix("EVENT_JSON:"))
        .map(|json| serde_json::from_str(json).unwrap())
        .collect()
}

fn panic_message(result: std::thread::Result<()>) -> String {
    let payload = result.expect_err("expected a panic");
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

// ---- Reservation, nonce, dispatch ----

#[test]
fn a_request_reserves_the_outlay_and_takes_the_next_nonce() {
    let mut minder = armed_evm();
    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(0));
    request(&mut minder, 1000);

    // Value plus the worst-case fee is what is reserved and counted.
    assert_eq!(reserved(&minder, evm_asset()), 1000 + FEE_COST);
    assert_eq!(committed(&minder, evm_asset()), 0);
    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(1));
}

#[test]
fn the_contract_sends_the_signer_a_hash_it_computed_itself() {
    let mut minder = armed_evm();
    request(&mut minder, 1000);

    let receipts = get_created_receipts();
    let sign = function_call(&receipts, "signer.near", "sign");
    let hash = evm::hex_encode(&evm::signing_hash(&expected_tx(0, 1000)));
    assert_eq!(
        sign.args,
        json!({
            "request": {
                "path": "minder-evm-1",
                "payload_v2": { "Ecdsa": hash },
                "domain_id": 0,
            }
        })
    );
    assert_eq!(sign.deposit, NearToken::from_yoctonear(1));
    assert!(sign.gas >= Gas::from_tgas(15), "the signer demands 15 Tgas");

    // The callback carries the asset, the accounted outlay and the exact
    // transaction that was hashed, borsh-encoded.
    let callback = function_call(&receipts, "minder.near", "on_sign_complete");
    assert!(callback.gas >= Gas::from_tgas(25));
    let mut expected_args = near_sdk::borsh::to_vec(&evm_asset()).unwrap();
    expected_args.extend(near_sdk::borsh::to_vec(&(1000 + FEE_COST)).unwrap());
    expected_args.extend(near_sdk::borsh::to_vec(&expected_tx(0, 1000)).unwrap());
    assert_eq!(callback.raw_args, expected_args);
}

#[test]
fn each_request_gets_its_own_nonce_and_hash() {
    let mut minder = armed_evm();
    request(&mut minder, 1000);
    request(&mut minder, 1000);

    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(2));
    assert_eq!(reserved(&minder, evm_asset()), 2 * (1000 + FEE_COST));
    let hashes: Vec<Value> = get_created_receipts()
        .iter()
        .flat_map(|receipt| receipt.actions.iter())
        .filter_map(|action| match action {
            near_sdk::mock::MockAction::FunctionCallWeight {
                method_name, args, ..
            } if method_name == b"sign" => {
                let args: Value = serde_json::from_slice(args).unwrap();
                Some(args["request"]["payload_v2"]["Ecdsa"].clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(hashes.len(), 2);
    assert_ne!(hashes[0], hashes[1]);
}

#[test]
fn a_second_request_sees_the_first_ones_reservation() {
    // TOCTOU: the first request is still in flight (no callback has run).
    let mut minder = armed_evm();
    request(&mut minder, 50_000_000_000_000); // outlay 92e12 of 150e12
    let second = minder.dry_run_evm(agent_key(), RECIPIENT.to_string(), U128(50_000_000_000_000));
    assert_eq!(second, Decision::Deny(DenyReason::WindowLimitExceeded));

    let attempt = catch_unwind(AssertUnwindSafe(|| {
        request(&mut minder, 50_000_000_000_000)
    }));
    assert!(panic_message(attempt).contains("WindowLimitExceeded"));
    assert_eq!(
        minder.get_evm_nonce(CHAIN_ID),
        U64(1),
        "a denial takes no nonce"
    );
}

// ---- Policy ----

#[test]
fn fees_count_toward_the_per_transaction_cap() {
    let mut minder = armed_evm();
    // The value alone is within the cap; value plus fee is over it.
    let value = EVM_PER_TX_CAP - FEE_COST + 1;
    assert!(value <= EVM_PER_TX_CAP);
    assert_eq!(
        minder.dry_run_evm(agent_key(), RECIPIENT.to_string(), U128(value)),
        Decision::Deny(DenyReason::PerTxCapExceeded)
    );
    assert_eq!(
        minder.dry_run_evm(agent_key(), RECIPIENT.to_string(), U128(value - 1)),
        Decision::Allow
    );
    let attempt = catch_unwind(AssertUnwindSafe(|| request(&mut minder, value)));
    assert!(panic_message(attempt).contains("PerTxCapExceeded"));
}

#[test]
fn many_tiny_payments_cannot_drain_through_fees() {
    // Each payment is 1 wei, but each costs a fee. The window limit stops them.
    let mut minder = armed_evm();
    let mut allowed = 0;
    while minder.dry_run_evm(agent_key(), RECIPIENT.to_string(), U128(1)) == Decision::Allow {
        request(&mut minder, 1);
        allowed += 1;
        assert!(allowed < 100, "the limit must bind");
    }
    assert_eq!(allowed, EVM_WINDOW_LIMIT / (1 + FEE_COST));
}

#[test]
fn the_evm_allowlist_is_separate_from_the_near_allowlist() {
    let minder = armed_evm();
    // bob.near is an allowed NEAR recipient; that says nothing about EVM.
    let other = "0x2222222222222222222222222222222222222222";
    assert_eq!(
        minder.dry_run_evm(agent_key(), other.to_string(), U128(1)),
        Decision::Deny(DenyReason::RecipientNotAllowed)
    );
    assert!(!minder.is_evm_recipient_allowed(other.to_string()));
    assert!(minder.is_evm_recipient_allowed(RECIPIENT.to_uppercase().replace("0X", "0x")));
    assert_eq!(
        minder.get_evm_recipients(None, None),
        vec![RECIPIENT.to_string()]
    );
}

#[test]
fn a_denied_request_changes_nothing() {
    let mut minder = armed_evm();
    testing_env!(context("mallory.near").build());
    let attempt = catch_unwind(AssertUnwindSafe(|| request(&mut minder, 1000)));
    assert!(panic_message(attempt).contains("NotAuthorized"));
    assert_eq!(reserved(&minder, evm_asset()), 0);
    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(0));
    assert!(get_created_receipts().is_empty());
}

#[test]
fn evm_payments_need_a_policy_a_config_and_a_valid_address() {
    let mut minder = armed_as_agent();
    // No config yet.
    let attempt = catch_unwind(AssertUnwindSafe(|| request(&mut minder, 1)));
    assert!(panic_message(attempt).contains("not configured"));

    let mut minder = armed_evm();
    let bad = catch_unwind(AssertUnwindSafe(|| {
        let _ = minder.request_evm_payment("0x1234".to_string(), U128(1));
    }));
    assert!(panic_message(bad).contains("Invalid EVM address"));

    // A config without a policy for the chain: the asset is not allowed.
    testing_env!(context("owner.near").build());
    minder.remove_asset_policy(evm_asset());
    testing_env!(agent_context().build());
    assert_eq!(
        minder.dry_run_evm(agent_key(), RECIPIENT.to_string(), U128(1)),
        Decision::Deny(DenyReason::AssetNotAllowed)
    );
}

#[test]
fn the_local_entry_points_refuse_evm_assets() {
    let mut minder = armed_evm();
    let attempt = catch_unwind(AssertUnwindSafe(|| {
        let _ = minder.request_local_transfer(evm_asset(), U128(1), account("bob.near"));
    }));
    assert!(panic_message(attempt).contains("request_evm_payment"));
    let dry = catch_unwind(AssertUnwindSafe(|| {
        let _ = minder.dry_run(agent_key(), evm_asset(), U128(1), account("bob.near"));
    }));
    assert!(panic_message(dry).contains("request_evm_payment"));
    testing_env!(context("owner.near").build());
    let withdraw = catch_unwind(AssertUnwindSafe(|| {
        let _ = minder.owner_withdraw(evm_asset(), U128(1), account("bob.near"));
    }));
    assert!(panic_message(withdraw).contains("request_evm_payment"));
}

// ---- Configuration ----

fn as_owner() {
    testing_env!(context("owner.near").build());
}

fn configure(
    minder: &mut Minder,
    signer: &str,
    path: &str,
    fee: u128,
    tip: u128,
    gas: Option<u64>,
) {
    minder.set_evm_config(
        account(signer),
        path.to_string(),
        test_sender_hex(),
        CHAIN_ID,
        U128(fee),
        U128(tip),
        gas,
    );
}

#[test]
#[should_panic(expected = "Only the owner can call this method")]
fn only_the_owner_sets_the_evm_config() {
    let mut minder = armed_evm();
    configure(
        &mut minder,
        "signer.near",
        "minder-evm-1",
        MAX_FEE,
        MAX_PRIORITY_FEE,
        None,
    );
}

#[test]
#[should_panic(expected = "Only the owner can call this method")]
fn only_the_owner_sets_evm_recipients() {
    let mut minder = armed_evm();
    minder.set_evm_recipient(
        "0x2222222222222222222222222222222222222222".to_string(),
        true,
    );
}

#[test]
#[should_panic(expected = "Only the owner can call this method")]
fn only_the_owner_raises_the_nonce() {
    let mut minder = armed_evm();
    minder.raise_evm_nonce(CHAIN_ID, U64(5));
}

#[test]
fn fees_and_chain_can_change_but_the_signing_identity_cannot() {
    let mut minder = armed_evm();
    as_owner();
    configure(
        &mut minder,
        "signer.near",
        "minder-evm-1",
        5,
        1,
        Some(50_000),
    );
    let config = minder.get_evm_config().unwrap();
    assert_eq!((config.max_fee_per_gas.0, config.gas_limit), (5, 50_000));

    let new_signer = catch_unwind(AssertUnwindSafe(|| {
        configure(&mut minder, "other-signer.near", "minder-evm-1", 5, 1, None);
    }));
    assert!(panic_message(new_signer).contains("cannot change once set"));
    let new_path = catch_unwind(AssertUnwindSafe(|| {
        configure(&mut minder, "signer.near", "another-path", 5, 1, None);
    }));
    assert!(panic_message(new_path).contains("cannot change once set"));
    let new_sender = catch_unwind(AssertUnwindSafe(|| {
        minder.set_evm_config(
            account("signer.near"),
            "minder-evm-1".to_string(),
            "0x70997970c51812dc3a010c7d01b50e0d17dc79c8".to_string(),
            CHAIN_ID,
            U128(5),
            U128(1),
            None,
        );
    }));
    assert!(panic_message(new_sender).contains("cannot change once set"));
    assert_eq!(
        minder.get_evm_config().unwrap().sender,
        test_sender_hex(),
        "the sender survived the attempt"
    );
}

#[test]
fn the_config_view_shows_the_sender_as_an_address() {
    let minder = armed_evm();
    let view = near_sdk::serde_json::to_value(minder.get_evm_config().unwrap()).unwrap();
    assert_eq!(view["sender"], test_sender_hex());
}

#[test]
fn the_sender_must_be_a_usable_address() {
    let mut minder = armed_as_agent();
    as_owner();
    for (label, sender) in [
        ("zero address", "0x0000000000000000000000000000000000000000"),
        ("too short", "0x9c9e702f"),
        ("not hex", "0xzz9e702f019f8354ddc1ce9a5ad0d95ce00e04c2"),
        ("empty", ""),
    ] {
        let result = catch_unwind(AssertUnwindSafe(|| {
            minder.set_evm_config(
                account("s.near"),
                "p".into(),
                sender.to_string(),
                CHAIN_ID,
                U128(1),
                U128(1),
                None,
            )
        }));
        assert!(result.is_err(), "{label} must be rejected");
    }
    assert!(minder.get_evm_config().is_none(), "nothing was stored");
}

#[test]
fn the_config_rejects_unusable_values() {
    let mut minder = armed_as_agent();
    as_owner();
    let rejected = |minder: &mut Minder, f: &dyn Fn(&mut Minder)| {
        catch_unwind(AssertUnwindSafe(|| f(minder))).is_err()
    };
    assert!(
        rejected(&mut minder, &|m| configure(m, "s.near", "p", 1, 2, None)),
        "tip above fee"
    );
    assert!(
        rejected(&mut minder, &|m| configure(m, "s.near", "p", 1, 1, Some(0))),
        "zero gas"
    );
    assert!(
        rejected(&mut minder, &|m| configure(m, "s.near", "", 1, 1, None)),
        "empty path"
    );
    assert!(
        rejected(&mut minder, &|m| m.set_evm_config(
            account("s.near"),
            "p".into(),
            test_sender_hex(),
            0,
            U128(1),
            U128(1),
            None
        )),
        "chain id 0"
    );
    assert!(
        rejected(&mut minder, &|m| configure(
            m,
            "s.near",
            "p",
            u128::MAX,
            1,
            None
        )),
        "fee times gas overflows"
    );
    assert!(minder.get_evm_config().is_none(), "nothing was stored");
}

#[test]
fn the_nonce_can_only_be_raised() {
    let mut minder = armed_evm();
    request(&mut minder, 1); // nonce 0 taken, next is 1
    as_owner();
    minder.raise_evm_nonce(CHAIN_ID, U64(7));
    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(7));
    minder.raise_evm_nonce(CHAIN_ID, U64(7)); // equal is fine
    let lower = catch_unwind(AssertUnwindSafe(|| {
        minder.raise_evm_nonce(CHAIN_ID, U64(6))
    }));
    assert!(panic_message(lower).contains("can only be raised"));
    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(7));
    // Other chains are independent.
    assert_eq!(minder.get_evm_nonce(1), U64(0));
}

// ---- Settlement ----

#[test]
fn a_returned_signature_settles_once_and_emits_the_signed_transaction() {
    let mut minder = armed_evm();
    request(&mut minder, 1000);
    let outlay = 1000 + FEE_COST;
    let logs = settle(
        &mut minder,
        outlay,
        0,
        1000,
        PromiseResult::Successful(valid_signature()),
    );

    assert_eq!(reserved(&minder, evm_asset()), 0);
    assert_eq!(
        committed(&minder, evm_asset()),
        outlay,
        "committed exactly once"
    );

    let events = event_lines(&logs);
    assert_eq!(events.len(), 1);
    let data = &events[0]["data"][0];
    // The reference serialization from the independent signer, not our encoder.
    let reference = fixture_for(0, 1000, Signer::Sender).raw_tx;
    assert_eq!(data["raw_tx"], format!("0x{}", evm::hex_encode(&reference)));
    assert_eq!(data["nonce"], 0);
    assert_eq!(data["to"], RECIPIENT);
    assert_eq!(data["value"], "1000");
}

// ---- Authenticity: the signature must recover to the configured sender ----

/// What settlement must do when the signature is well formed but does not
/// recover to the sender: book the outlay as for any successful receipt, emit
/// no transaction, and say why.
fn assert_booked_but_not_emitted(minder: &Minder, logs: &[String], outlay: u128) {
    assert_eq!(reserved(minder, evm_asset()), 0);
    assert_eq!(committed(minder, evm_asset()), outlay, "booked per G1");
    assert!(event_lines(logs).is_empty(), "nothing emitted");
    let line = logs
        .iter()
        .find(|line| line.contains("authenticity failure"))
        .expect("an authenticity-failure line is logged");
    assert!(line.contains("signer or configuration fault"));
}

#[test]
fn a_signature_by_another_key_is_booked_but_not_emitted() {
    let mut minder = armed_evm();
    request(&mut minder, 1000);
    let outlay = 1000 + FEE_COST;
    let other = signer_response(&fixture_for(0, 1000, Signer::Other));
    let logs = settle(
        &mut minder,
        outlay,
        0,
        1000,
        PromiseResult::Successful(other),
    );
    assert_booked_but_not_emitted(&minder, &logs, outlay);
    let line = logs
        .iter()
        .find(|l| l.contains("authenticity failure"))
        .unwrap();
    assert!(
        line.contains(&test_sender_hex()),
        "names the configured sender"
    );
}

#[test]
fn a_signature_over_a_different_transaction_is_booked_but_not_emitted() {
    let mut minder = armed_evm();
    request(&mut minder, 1000);
    let outlay = 1000 + FEE_COST;
    // The sender's own signature, but over the nonce-1 transaction.
    let wrong_hash = signer_response(&fixture_for(1, 1000, Signer::Sender));
    let logs = settle(
        &mut minder,
        outlay,
        0,
        1000,
        PromiseResult::Successful(wrong_hash),
    );
    assert_booked_but_not_emitted(&minder, &logs, outlay);
}

#[test]
fn a_signature_that_recovers_to_nothing_is_booked_but_not_emitted() {
    let mut minder = armed_evm();
    request(&mut minder, 1000);
    let outlay = 1000 + FEE_COST;
    // x = 5 is not the x coordinate of any secp256k1 point, so no key recovers.
    let real = fixture_for(0, 1000, Signer::Sender);
    let off_curve = signature(
        &format!("02{}", "00".repeat(31) + "05"),
        &evm::hex_encode(&real.s),
        0,
    );
    let logs = settle(
        &mut minder,
        outlay,
        0,
        1000,
        PromiseResult::Successful(off_curve),
    );
    assert_booked_but_not_emitted(&minder, &logs, outlay);
    assert!(logs.iter().any(|l| l.contains("recovers to no address")));
}

#[test]
fn a_flipped_recovery_id_is_booked_but_not_emitted() {
    let mut minder = armed_evm();
    request(&mut minder, 1000);
    let outlay = 1000 + FEE_COST;
    let real = fixture_for(0, 1000, Signer::Sender);
    let flipped = signature(
        &format!("02{}", evm::hex_encode(&real.r)),
        &evm::hex_encode(&real.s),
        real.y_parity ^ 1,
    );
    let logs = settle(
        &mut minder,
        outlay,
        0,
        1000,
        PromiseResult::Successful(flipped),
    );
    assert_booked_but_not_emitted(&minder, &logs, outlay);
}

#[test]
fn a_sender_that_differs_from_the_signing_key_blocks_emission() {
    // The configuration fault: the sender set is not the address the signer's
    // key derives to, so even the signer's genuine signatures are not emitted.
    let mut minder = armed_as_agent();
    testing_env!(context("owner.near").build());
    minder.set_asset_policy(
        evm_asset(),
        U128(EVM_PER_TX_CAP),
        U128(EVM_WINDOW_LIMIT),
        None,
    );
    minder.set_evm_recipient(RECIPIENT.to_string(), true);
    minder.set_evm_config(
        account("signer.near"),
        "minder-evm-1".to_string(),
        RECIPIENT.to_string(),
        CHAIN_ID,
        U128(MAX_FEE),
        U128(MAX_PRIORITY_FEE),
        None,
    );
    testing_env!(agent_context().build());
    request(&mut minder, 1000);
    let outlay = 1000 + FEE_COST;
    let logs = settle(
        &mut minder,
        outlay,
        0,
        1000,
        PromiseResult::Successful(valid_signature()),
    );
    assert_booked_but_not_emitted(&minder, &logs, outlay);
}

#[test]
fn a_failed_signer_call_releases_and_commits_nothing() {
    let mut minder = armed_evm();
    let before = minder.dry_run_evm(agent_key(), RECIPIENT.to_string(), U128(1000));
    request(&mut minder, 1000);
    let logs = settle(&mut minder, 1000 + FEE_COST, 0, 1000, PromiseResult::Failed);

    assert_eq!(reserved(&minder, evm_asset()), 0);
    assert_eq!(committed(&minder, evm_asset()), 0);
    assert!(event_lines(&logs).is_empty());
    assert_eq!(
        minder.dry_run_evm(agent_key(), RECIPIENT.to_string(), U128(1000)),
        before
    );
    // The nonce stays consumed: the documented outbound-only gap.
    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(1));
}

/// A genuine, assemblable signature padded past the contract's read buffer: the
/// bytes are unreadable to the contract but the signer did sign.
fn oversized_valid_signature() -> Vec<u8> {
    let signature = fixture_for(0, 1000, Signer::Sender);
    let mut response: Value = serde_json::from_slice(&signer_response(&signature)).unwrap();
    response["padding"] = json!("x".repeat(5000));
    response.to_string().into_bytes()
}

#[test]
fn a_successful_receipt_we_cannot_use_is_booked_but_emits_nothing() {
    // G1: the signer's receipt succeeded, so whatever it returned is public and
    // possibly broadcastable. The outlay is booked whether or not the contract
    // could read, parse or assemble it; only the event depends on that.
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("recovery id 2", signature(&format!("02{R}"), S, 2)),
        ("zero s", signature(&format!("02{R}"), &"00".repeat(32), 0)),
        (
            "s at the order",
            signature(
                &format!("02{R}"),
                "fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141",
                0,
            ),
        ),
        ("bad point prefix", signature(&format!("04{R}"), S, 0)),
        ("short point", signature("02ab", S, 0)),
        ("not hex", signature("zz", S, 0)),
        (
            "wrong shape",
            br#"{"big_r":"02","s":"00","recovery_id":1}"#.to_vec(),
        ),
        ("not json", b"not json".to_vec()),
        ("empty", Vec::new()),
        ("oversized garbage", vec![b'x'; 5000]),
        ("oversized valid signature", oversized_valid_signature()),
    ];
    for (name, payload) in cases {
        clear_storage();
        let mut minder = armed_evm();
        request(&mut minder, 1000);
        let logs = settle(
            &mut minder,
            1000 + FEE_COST,
            0,
            1000,
            PromiseResult::Successful(payload),
        );

        assert_eq!(reserved(&minder, evm_asset()), 0, "{name}: released");
        assert_eq!(
            committed(&minder, evm_asset()),
            1000 + FEE_COST,
            "{name}: booked"
        );
        assert!(event_lines(&logs).is_empty(), "{name}: nothing emitted");
        assert!(
            logs.iter()
                .any(|line| line.contains("booked, no transaction emitted")),
            "{name}: the outcome is logged"
        );
    }
}

#[test]
fn an_unreadable_but_successful_result_is_logged_as_unreadable() {
    let mut minder = armed_evm();
    request(&mut minder, 1000);
    let logs = settle(
        &mut minder,
        1000 + FEE_COST,
        0,
        1000,
        PromiseResult::Successful(oversized_valid_signature()),
    );
    assert!(logs
        .iter()
        .any(|line| line.contains("unreadable signer result")));
}

#[test]
fn booked_unusable_output_counts_against_the_window() {
    let mut minder = armed_evm();
    // Two requests whose results we cannot use. Each books 42e12 + 1000.
    for nonce in 0..2 {
        request(&mut minder, 1000);
        settle(
            &mut minder,
            1000 + FEE_COST,
            nonce,
            1000,
            PromiseResult::Successful(b"garbage".to_vec()),
        );
    }
    assert_eq!(committed(&minder, evm_asset()), 2 * (1000 + FEE_COST));
    // Limit 150e12, 84e12+2000 booked: a payment with outlay 62e12 fits, one
    // with outlay 92e12 does not.
    let ask = |value: u128| minder.dry_run_evm(agent_key(), RECIPIENT.to_string(), U128(value));
    assert_eq!(ask(20_000_000_000_000), Decision::Allow);
    assert_eq!(
        ask(50_000_000_000_000),
        Decision::Deny(DenyReason::WindowLimitExceeded)
    );
}

#[test]
fn only_a_failed_receipt_books_nothing() {
    // The converse of the above, and the only unbooked outcome.
    let mut minder = armed_evm();
    request(&mut minder, 1000);
    let logs = settle(&mut minder, 1000 + FEE_COST, 0, 1000, PromiseResult::Failed);
    assert_eq!(reserved(&minder, evm_asset()), 0);
    assert_eq!(committed(&minder, evm_asset()), 0);
    assert!(event_lines(&logs).is_empty());
    assert!(logs.iter().any(|line| line.contains("not signed")));
}

#[test]
fn a_high_s_signature_is_normalized_before_the_transaction_is_emitted() {
    let mut minder = armed_evm();
    request(&mut minder, 1000);
    // n - s with the opposite parity is the same signature in high-s form.
    let real = fixture_for(0, 1000, Signer::Sender);
    let n_minus_s = {
        let order = hex_array("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141");
        let s = real.s;
        let mut out = [0u8; 32];
        let mut borrow = 0i16;
        for i in (0..32).rev() {
            let mut digit = i16::from(order[i]) - i16::from(s[i]) - borrow;
            borrow = i16::from(digit < 0);
            if digit < 0 {
                digit += 256;
            }
            out[i] = digit as u8;
        }
        evm::hex_encode(&out)
    };
    let logs = settle(
        &mut minder,
        1000 + FEE_COST,
        0,
        1000,
        PromiseResult::Successful(signature(
            &format!("02{}", evm::hex_encode(&real.r)),
            &n_minus_s,
            real.y_parity ^ 1,
        )),
    );
    // Emitted in the canonical low-s form, byte for byte as the reference signer made it.
    assert_eq!(
        event_lines(&logs)[0]["data"][0]["raw_tx"],
        format!("0x{}", evm::hex_encode(&real.raw_tx))
    );
}

#[test]
fn settlement_never_panics_even_if_it_releases_more_than_is_reserved() {
    let mut minder = armed_evm();
    request(&mut minder, 10);
    settle(
        &mut minder,
        5 * (10 + FEE_COST),
        0,
        10,
        PromiseResult::Failed,
    );
    assert_eq!(reserved(&minder, evm_asset()), 0);
    // And with nothing reserved at all.
    settle(&mut minder, 1, 0, 10, PromiseResult::Failed);
    assert_eq!(reserved(&minder, evm_asset()), 0);
}

#[test]
fn settlement_is_independent_of_changes_made_in_flight() {
    let mut minder = armed_evm();
    request(&mut minder, 1000);
    as_owner();
    minder.freeze();
    minder.remove_asset_policy(evm_asset());
    minder.set_evm_recipient(RECIPIENT.to_string(), false);
    settle(
        &mut minder,
        1000 + FEE_COST,
        0,
        1000,
        PromiseResult::Successful(valid_signature()),
    );
    assert_eq!(reserved(&minder, evm_asset()), 0);
    assert_eq!(committed(&minder, evm_asset()), 1000 + FEE_COST);
}

fn hex_array(text: &str) -> [u8; 32] {
    evm::hex_decode(text).unwrap().try_into().unwrap()
}

// ---- Owner recovery: owner_evm_withdraw ----

/// An address no policy allows.
const NOT_ALLOWLISTED: &str = "0x00000000000000000000000000000000000000aa";
/// Far over every cap and the window limit.
const HUGE: u128 = 5_000_000_000_000_000;

fn owner_tx(nonce: u64, to: &str, value: u128) -> Eip1559Tx {
    Eip1559Tx {
        to: evm::parse_address(to).unwrap(),
        ..expected_tx(nonce, value)
    }
}

fn signer_response_for(tx: &Eip1559Tx, signer: Signer) -> Vec<u8> {
    signer_response(&fixture_signature(&evm::signing_hash(tx), signer))
}

fn owner_withdraw(minder: &mut Minder, to: &str, value: u128, nonce: u64) {
    as_owner();
    let _ = minder.owner_evm_withdraw(to.to_string(), U128(value), U64(nonce));
}

/// Runs the owner callback as the contract with the signer's result and
/// returns what it logged.
fn settle_owner(minder: &Minder, tx: Eip1559Tx, result: PromiseResult) -> Vec<String> {
    testing_env!(
        context("minder.near").build(),
        near_sdk::test_vm_config(),
        RuntimeFeesConfig::test(),
        Default::default(),
        vec![result]
    );
    minder.on_owner_sign_complete(tx);
    let logs = get_logs();
    testing_env!(agent_context().build());
    logs
}

fn owner_events(logs: &[String]) -> Vec<Value> {
    event_lines(logs)
        .into_iter()
        .inspect(|event| assert_eq!(event["event"], "evm_owner_withdrawal_signed"))
        .collect()
}

#[test]
fn only_the_owner_can_withdraw_cross_chain() {
    let mut minder = armed_evm();
    // The agent's own key, the contract's account and a stranger.
    for context in [
        agent_context(),
        super::test_support::context("minder.near"),
        super::test_support::context("bob.near"),
    ] {
        testing_env!(context.build());
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = minder.owner_evm_withdraw(RECIPIENT.to_string(), U128(1), U64(0));
        }));
        assert!(panic_message(result).contains("Only the owner can call this method"));
    }
    assert!(
        get_created_receipts().is_empty(),
        "nothing reached the signer"
    );
    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(0));
}

#[test]
fn it_needs_the_evm_configuration() {
    testing_env!(context("owner.near").build());
    let mut minder = Minder::init(account("owner.near"), U64(400), 4);
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _ = minder.owner_evm_withdraw(RECIPIENT.to_string(), U128(1), U64(0));
    }));
    assert!(panic_message(result).contains("not configured"));
}

#[test]
fn it_rejects_a_malformed_recipient_and_an_exhausted_nonce() {
    let mut minder = armed_evm();
    as_owner();
    for (recipient, nonce) in [("0x1234", 0), (RECIPIENT, u64::MAX)] {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = minder.owner_evm_withdraw(recipient.to_string(), U128(1), U64(nonce));
        }));
        assert!(result.is_err(), "{recipient} at {nonce}");
    }
    assert!(get_created_receipts().is_empty());
    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(0));
}

#[test]
fn it_works_while_frozen_and_emits_the_signed_transaction() {
    let mut minder = armed_evm();
    as_owner();
    minder.freeze();
    // The agent is stopped...
    testing_env!(agent_context().build());
    assert_eq!(
        minder.dry_run_evm(agent_key(), RECIPIENT.to_string(), U128(1)),
        Decision::Deny(DenyReason::Frozen)
    );

    // ...the owner is not.
    owner_withdraw(&mut minder, RECIPIENT, 1000, 0);
    let tx = owner_tx(0, RECIPIENT, 1000);
    let logs = settle_owner(
        &minder,
        tx.clone(),
        PromiseResult::Successful(signer_response_for(&tx, Signer::Sender)),
    );

    let events = owner_events(&logs);
    assert_eq!(events.len(), 1);
    let data = &events[0]["data"][0];
    let reference = fixture_signature(&evm::signing_hash(&tx), Signer::Sender).raw_tx;
    assert_eq!(data["raw_tx"], format!("0x{}", evm::hex_encode(&reference)));
    assert_eq!(
        (data["nonce"].as_u64(), data["value"].as_str()),
        (Some(0), Some("1000"))
    );
}

#[test]
fn it_works_with_the_mandate_revoked_and_no_policy() {
    let mut minder = armed_evm();
    as_owner();
    minder.revoke_mandate();
    minder.remove_asset_policy(evm_asset());
    owner_withdraw(&mut minder, RECIPIENT, 1000, 0);
    assert_eq!(
        get_created_receipts()
            .iter()
            .filter(|r| r.receiver_id == account("signer.near"))
            .count(),
        1
    );
}

#[test]
fn it_is_outside_policy_and_touches_neither_reserved_nor_the_window() {
    let mut minder = armed_evm();
    // Something in flight and something committed, so unchanged is not just zero.
    request(&mut minder, 1000);
    settle(
        &mut minder,
        1000 + FEE_COST,
        0,
        1000,
        PromiseResult::Successful(valid_signature()),
    );
    request(&mut minder, 2000);
    let before = (
        reserved(&minder, evm_asset()),
        committed(&minder, evm_asset()),
        minder.dry_run_evm(agent_key(), RECIPIENT.to_string(), U128(1000)),
    );
    assert!(before.0 > 0 && before.1 > 0);

    // Far over the per-transaction cap and the window limit, to an address that
    // is on no allowlist.
    owner_withdraw(&mut minder, NOT_ALLOWLISTED, HUGE, 3);
    let tx = owner_tx(3, NOT_ALLOWLISTED, HUGE);
    let sign = function_call(&get_created_receipts(), "signer.near", "sign");
    assert_eq!(
        sign.args["request"]["payload_v2"]["Ecdsa"],
        evm::hex_encode(&evm::signing_hash(&tx))
    );
    const { assert!(HUGE > EVM_PER_TX_CAP && HUGE > EVM_WINDOW_LIMIT) };

    let during = (
        reserved(&minder, evm_asset()),
        committed(&minder, evm_asset()),
    );
    assert_eq!(
        (during.0, during.1),
        (before.0, before.1),
        "dispatch changed accounting"
    );

    // And the callback books nothing either, whether it emits or not.
    for result in [
        PromiseResult::Successful(signer_response_for(&tx, Signer::Sender)),
        PromiseResult::Successful(signer_response_for(&tx, Signer::Other)),
        PromiseResult::Failed,
    ] {
        settle_owner(&minder, tx.clone(), result);
        assert_eq!(
            (
                reserved(&minder, evm_asset()),
                committed(&minder, evm_asset()),
                minder.dry_run_evm(agent_key(), RECIPIENT.to_string(), U128(1000)),
            ),
            before
        );
    }
}

#[test]
fn the_explicit_nonce_is_used_and_the_counter_moves_to_max_of_counter_and_nonce_plus_one() {
    let mut minder = armed_evm();
    let sent_nonce = |expected: u64| {
        let receipts = get_created_receipts();
        let callback = function_call(&receipts, "minder.near", "on_owner_sign_complete");
        let sign = function_call(&receipts, "signer.near", "sign");
        let tx = owner_tx(expected, RECIPIENT, 1);
        assert_eq!(callback.raw_args, near_sdk::borsh::to_vec(&tx).unwrap());
        assert_eq!(
            sign.args["request"]["payload_v2"]["Ecdsa"],
            evm::hex_encode(&evm::signing_hash(&tx))
        );
    };

    // A future nonce advances the counter past it.
    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(0));
    owner_withdraw(&mut minder, RECIPIENT, 1, 5);
    sent_nonce(5);
    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(6));

    // A replacement of an issued nonce leaves it alone.
    owner_withdraw(&mut minder, RECIPIENT, 1, 2);
    sent_nonce(2);
    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(6));
    owner_withdraw(&mut minder, RECIPIENT, 1, 0);
    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(6));

    // The nonce the agent would take next advances it by one; beyond it, further.
    owner_withdraw(&mut minder, RECIPIENT, 1, 6);
    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(7));
    owner_withdraw(&mut minder, RECIPIENT, 1, 7);
    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(8));

    // The agent then takes the next free nonce, never one the owner used.
    testing_env!(agent_context().build());
    request(&mut minder, 1000);
    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(9));
    let receipts = get_created_receipts();
    let hash = evm::hex_encode(&evm::signing_hash(&expected_tx(8, 1000)));
    assert_eq!(
        function_call(&receipts, "signer.near", "sign").args["request"]["payload_v2"]["Ecdsa"],
        hash
    );
}

#[test]
fn the_signer_is_sent_the_hash_of_the_transaction_the_contract_built() {
    let mut minder = armed_evm();
    owner_withdraw(&mut minder, NOT_ALLOWLISTED, 777, 4);
    let receipts = get_created_receipts();
    let tx = owner_tx(4, NOT_ALLOWLISTED, 777);

    let sign = function_call(&receipts, "signer.near", "sign");
    assert_eq!(
        sign.args,
        json!({
            "request": {
                "path": "minder-evm-1",
                "payload_v2": { "Ecdsa": evm::hex_encode(&evm::signing_hash(&tx)) },
                "domain_id": 0,
            }
        })
    );
    assert_eq!(sign.deposit, NearToken::from_yoctonear(1));
    assert!(sign.gas >= Gas::from_tgas(15), "the signer demands 15 Tgas");
    // The transaction carries the configured chain and fees, the owner's
    // recipient, value and nonce, and nothing else.
    assert_eq!(tx.max_fee_per_gas, MAX_FEE);
    assert_eq!(tx.chain_id, CHAIN_ID);

    let callback = function_call(&receipts, "minder.near", "on_owner_sign_complete");
    assert!(callback.gas >= Gas::from_tgas(25));
    assert_eq!(callback.raw_args, near_sdk::borsh::to_vec(&tx).unwrap());
}

#[test]
fn a_cancellation_is_a_zero_value_transfer_to_the_sender_itself() {
    let mut minder = armed_evm();
    request(&mut minder, 1000); // the agent's transaction at nonce 0, say it is stuck
    owner_withdraw(&mut minder, &test_sender_hex(), 0, 0);
    assert_eq!(
        minder.get_evm_nonce(CHAIN_ID),
        U64(1),
        "a replacement leaves the counter"
    );
    let tx = owner_tx(0, &test_sender_hex(), 0);
    let logs = settle_owner(
        &minder,
        tx.clone(),
        PromiseResult::Successful(signer_response_for(&tx, Signer::Sender)),
    );
    assert_eq!(owner_events(&logs).len(), 1);
}

#[test]
fn a_signature_by_another_key_is_not_emitted_for_the_owner_either() {
    let mut minder = armed_evm();
    owner_withdraw(&mut minder, RECIPIENT, 1000, 0);
    let tx = owner_tx(0, RECIPIENT, 1000);
    let logs = settle_owner(
        &minder,
        tx.clone(),
        PromiseResult::Successful(signer_response_for(&tx, Signer::Other)),
    );
    assert!(event_lines(&logs).is_empty());
    let line = logs
        .iter()
        .find(|line| line.contains("authenticity failure"))
        .expect("an authenticity failure is logged");
    assert!(line.contains("signer or configuration fault"));
    assert!(line.starts_with("owner evm withdrawal not emitted"));
}

#[test]
fn a_mismatched_sender_configuration_emits_nothing_for_the_owner() {
    let mut minder = armed_as_agent();
    testing_env!(context("owner.near").build());
    minder.set_evm_config(
        account("signer.near"),
        "minder-evm-1".to_string(),
        NOT_ALLOWLISTED.to_string(), // not the key's address
        CHAIN_ID,
        U128(MAX_FEE),
        U128(MAX_PRIORITY_FEE),
        None,
    );
    let _ = minder.owner_evm_withdraw(RECIPIENT.to_string(), U128(1000), U64(0));
    let tx = owner_tx(0, RECIPIENT, 1000);
    let logs = settle_owner(
        &minder,
        tx.clone(),
        PromiseResult::Successful(signer_response_for(&tx, Signer::Sender)),
    );
    assert!(event_lines(&logs).is_empty());
    assert!(logs.iter().any(|l| l.contains("authenticity failure")));
}

#[test]
fn unusable_signer_results_emit_nothing_and_book_nothing_for_the_owner() {
    let mut minder = armed_evm();
    owner_withdraw(&mut minder, RECIPIENT, 1000, 0);
    let tx = owner_tx(0, RECIPIENT, 1000);
    let cases = [
        (PromiseResult::Failed, "not signed"),
        (
            PromiseResult::Successful(b"garbage".to_vec()),
            "signature rejected",
        ),
        (
            PromiseResult::Successful(signature(&format!("02{R}"), S, 2)),
            "signature rejected",
        ),
        (
            PromiseResult::Successful(vec![b'x'; 5000]),
            "unreadable signer result",
        ),
    ];
    for (result, expected) in cases {
        let logs = settle_owner(&minder, tx.clone(), result);
        assert!(event_lines(&logs).is_empty(), "{expected}");
        assert!(
            logs.iter().any(|line| line.contains(expected)),
            "{expected}: {logs:?}"
        );
        assert_eq!(committed(&minder, evm_asset()), 0);
    }
}

// ---- Mandate scope on the EVM surface ----

use crate::MandateScope;

/// Replaces the mandate with one for `other_key()` carrying `scope`, and acts
/// as that key.
fn scoped_agent(minder: &mut Minder, scope: MandateScope) {
    as_owner();
    minder.grant_mandate(other_key(), U64(u64::MAX), Some(scope));
    testing_env!(agent_context_for(other_key()).build());
}

fn evm_decision(minder: &Minder, to: &str, value: u128) -> Decision {
    minder.dry_run_evm(other_key(), to.to_string(), U128(value))
}

/// The reason a real request is denied for, or `None` if it is accepted.
fn evm_request_denial(minder: &mut Minder, to: &str, value: u128) -> Option<String> {
    catch_unwind(AssertUnwindSafe(|| {
        let _ = minder.request_evm_payment(to.to_string(), U128(value));
    }))
    .err()
    .map(|payload| {
        payload
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default()
    })
}

#[test]
fn an_asset_scope_that_excludes_the_evm_asset_denies_evm_payments() {
    let mut minder = armed_evm();
    scoped_agent(
        &mut minder,
        MandateScope {
            allowed_assets: Some(vec![AssetId::Near]),
            ..MandateScope::default()
        },
    );
    assert_eq!(
        evm_decision(&minder, RECIPIENT, 1000),
        Decision::Deny(DenyReason::AssetOutOfScope)
    );
    let denial = evm_request_denial(&mut minder, RECIPIENT, 1000).expect("denied");
    assert!(denial.contains("Denied: AssetOutOfScope"), "{denial}");
    assert_eq!(
        reserved(&minder, evm_asset()),
        0,
        "a denied request reserves nothing"
    );
    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(0), "and takes no nonce");
}

#[test]
fn an_evm_recipient_scope_is_enforced_after_the_global_allowlist() {
    let mut minder = armed_evm();
    as_owner();
    // A second, globally allowed recipient; and a scope naming the first (in
    // upper case, to show the entry is matched normalized) and an address that
    // is on no allowlist.
    minder.set_evm_recipient(NOT_ALLOWLISTED.to_string(), true);
    let unlisted = "0x00000000000000000000000000000000000000bb";
    scoped_agent(
        &mut minder,
        MandateScope {
            allowed_assets: Some(vec![evm_asset()]),
            allowed_evm_recipients: Some(vec![
                format!("0x{}", RECIPIENT[2..].to_uppercase()),
                unlisted.to_string(),
            ]),
            ..MandateScope::default()
        },
    );
    assert_eq!(evm_decision(&minder, RECIPIENT, 1000), Decision::Allow);
    assert_eq!(
        evm_decision(&minder, NOT_ALLOWLISTED, 1000),
        Decision::Deny(DenyReason::RecipientOutOfScope),
        "globally allowed, outside the scope"
    );
    assert_eq!(
        evm_decision(&minder, unlisted, 1000),
        Decision::Deny(DenyReason::RecipientNotAllowed),
        "in the scope, not globally allowed: the global check decides"
    );

    assert!(evm_request_denial(&mut minder, RECIPIENT, 1000).is_none());
    let denial = evm_request_denial(&mut minder, NOT_ALLOWLISTED, 1000).expect("denied");
    assert!(denial.contains("Denied: RecipientOutOfScope"), "{denial}");
}

#[test]
fn the_mandate_cap_applies_to_the_evm_outlay_and_only_ever_narrows() {
    let mut minder = armed_evm(); // global per-transaction cap 1e14, outlay = value + 4.2e13
    scoped_agent(
        &mut minder,
        MandateScope {
            per_tx_cap: Some(U128(FEE_COST + 500)),
            ..MandateScope::default()
        },
    );
    assert_eq!(evm_decision(&minder, RECIPIENT, 500), Decision::Allow);
    assert_eq!(
        evm_decision(&minder, RECIPIENT, 501),
        Decision::Deny(DenyReason::PerTxCapExceeded)
    );

    // Looser than the global cap: the global one still binds.
    scoped_agent_after_revoke(
        &mut minder,
        MandateScope {
            per_tx_cap: Some(U128(u128::MAX)),
            ..MandateScope::default()
        },
    );
    assert_eq!(
        evm_decision(&minder, RECIPIENT, EVM_PER_TX_CAP - FEE_COST),
        Decision::Allow
    );
    assert_eq!(
        evm_decision(&minder, RECIPIENT, EVM_PER_TX_CAP - FEE_COST + 1),
        Decision::Deny(DenyReason::PerTxCapExceeded)
    );
}

/// As `scoped_agent`, for a mandate that is replaced again: revoked first, so
/// its key may be granted once more.
fn scoped_agent_after_revoke(minder: &mut Minder, scope: MandateScope) {
    as_owner();
    minder.revoke_mandate();
    scoped_agent(minder, scope);
}

#[test]
fn an_unscoped_mandate_leaves_the_evm_surface_as_it_was() {
    let mut minder = armed_evm();
    scoped_agent(&mut minder, MandateScope::default());
    assert_eq!(evm_decision(&minder, RECIPIENT, 1000), Decision::Allow);
    assert!(evm_request_denial(&mut minder, RECIPIENT, 1000).is_none());
    assert_eq!(minder.get_evm_nonce(CHAIN_ID), U64(1));
}

// ---- Lifetime budget on the EVM surface ----

#[test]
fn the_evm_budget_counts_outlays_in_flight_and_binds_before_the_window() {
    let mut minder = armed_evm();
    // Window 1.5e14, per-transaction cap 1e14; budget 1e14. One payment's
    // outlay is 4.2e13 plus its value, so two fit the budget and three do not.
    as_owner();
    minder.set_asset_policy(
        evm_asset(),
        U128(EVM_PER_TX_CAP),
        U128(EVM_WINDOW_LIMIT),
        Some(U128(100_000_000_000_000)),
    );
    testing_env!(agent_context().build());

    assert_eq!(
        minder.dry_run_evm(agent_key(), RECIPIENT.to_string(), U128(1000)),
        Decision::Allow
    );
    request(&mut minder, 1000);
    request(&mut minder, 1000);
    // Both are still in flight, nothing is committed: the reserved outlays
    // alone are what the third meets, and the window (1.5e14) would allow it.
    assert_eq!(committed(&minder, evm_asset()), 0);
    assert_eq!(
        minder.dry_run_evm(agent_key(), RECIPIENT.to_string(), U128(1000)),
        Decision::Deny(DenyReason::MandateBudgetExceeded)
    );
    let denied = catch_unwind(AssertUnwindSafe(|| {
        let _ = minder.request_evm_payment(RECIPIENT.to_string(), U128(1000));
    }));
    assert!(panic_message(denied).contains("Denied: MandateBudgetExceeded"));
    assert_eq!(
        minder.get_evm_nonce(CHAIN_ID),
        U64(2),
        "a denied request takes no nonce"
    );
}
