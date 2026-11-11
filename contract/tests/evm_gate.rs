//! The EVM signing gate in a sandbox, against `test-contracts/mock-signer`:
//! reservation, the hash the signer is sent, settlement of a returned
//! signature, and release on failure. Expected hashes and raw transactions are
//! viem's (`vectors/gen-gate-vectors.mjs`), not this crate's. See DESIGN.md for
//! the invariant map.

mod common;

use std::time::Duration;

use common::*;
use minder::{Decision, DenyReason};
use near_sdk::serde_json::{json, Value};
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::Gas;
use near_workspaces::{AccountId, Contract};

const CHAIN_ID: u64 = 11_155_111;
const PATH: &str = "minder-evm-1";
const RECIPIENT: &str = "0x70997970c51812dc3a010c7d01b50e0d17dc79c8";
const VALUE: u128 = 1000;
/// 21000 gas at 2 gwei: what each payment's fee is accounted at.
const FEE_COST: u128 = 42_000_000_000_000;
const OUTLAY: u128 = VALUE + FEE_COST;
const EVM_PER_TX_CAP: u128 = 100_000_000_000_000;
const EVM_WINDOW_LIMIT: u128 = 120_000_000_000_000;

// viem's signing hash for the configuration above, at nonces 0 and 1. The
// signatures over them, and the signed transactions, are from the committed
// fixture (`vectors/evm-signatures.json`): an independent signer's, so the
// mock signer returns signatures that genuinely recover for these hashes.
const HASH_NONCE_0: &str = "1a75e78121e9d9ed1cf712c1ddf771430881926816812e670a29c94e84d8d138";
const HASH_NONCE_1: &str = "4af5cb1e337b3b9b7242504bc4122134331a7bddade33cf6c67adf609b3497cc";
const HASH_NONCE_5: &str = "6bbc3bfb4a0955cccc4e0009eb44aa1dfc02975e9ba2127d70829651c2e32dc4";

/// viem's serialization of the sender-signed transaction for a signing hash.
fn raw_tx(hash: &str) -> String {
    fixture_signature(hash, "sender").raw_tx
}

fn evm() -> Value {
    json!({ "Evm": CHAIN_ID })
}

/// The fixture armed for NEAR, plus an EVM policy, an allowed EVM recipient
/// and an EVM config pointing at a mock signer.
async fn armed_evm() -> (Fixture, Contract) {
    let f = Fixture::armed().await;
    let signer = f.deploy_signer().await;
    f.register_signatures(&signer, &[HASH_NONCE_0, HASH_NONCE_1, HASH_NONCE_5])
        .await;
    f.set_policy(evm(), EVM_PER_TX_CAP, EVM_WINDOW_LIMIT).await;
    f.owner_ok(
        "set_evm_recipient",
        json!({ "address": RECIPIENT, "allowed": true }),
    )
    .await;
    f.owner_ok(
        "set_evm_config",
        json!({
            "signer_id": signer.id(),
            "derivation_path": PATH,
            "sender": fixture_sender(),
            "chain_id": CHAIN_ID,
            "max_fee_per_gas": "2000000000",
            "max_priority_fee_per_gas": "1000000000",
            "gas_limit": null,
        }),
    )
    .await;
    (f, signer)
}

async fn pay(
    f: &Fixture,
    who: &near_workspaces::Account,
    recipient: &str,
    value: u128,
) -> ExecutionFinalResult {
    who.call(f.contract.id(), "request_evm_payment")
        .args_json(json!({ "recipient": recipient, "amount": value.to_string() }))
        .gas(Gas::from_tgas(AGENT_TGAS))
        .transact()
        .await
        .unwrap()
}

async fn dry_run_evm(f: &Fixture, value: u128) -> Decision {
    f.view(
        "dry_run_evm",
        json!({ "agent_key": f.agent_key, "recipient": RECIPIENT, "amount": value.to_string() }),
    )
    .await
}

async fn set_mode(signer: &Contract, mode: &str) {
    signer
        .call("set_mode")
        .args_json(json!({ "mode": mode }))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
}

async fn signer_requests(signer: &Contract) -> Vec<Value> {
    signer.view("get_requests").await.unwrap().json().unwrap()
}

async fn nonce(f: &Fixture) -> String {
    f.view("get_evm_nonce", json!({ "chain_id": CHAIN_ID }))
        .await
}

/// The signed-payment events in a result's logs.
fn events(result: &ExecutionFinalResult) -> Vec<Value> {
    result
        .logs()
        .iter()
        .filter_map(|line| line.strip_prefix("EVENT_JSON:"))
        .map(|json| near_sdk::serde_json::from_str(json).unwrap())
        .collect()
}

fn logs_contain(result: &ExecutionFinalResult, needle: &str) -> bool {
    result.logs().iter().any(|line| line.contains(needle))
}

// ---- Settlement of a returned signature ----

#[tokio::test]
async fn a_returned_signature_settles_once_and_emits_the_signed_transaction() {
    let (f, signer) = armed_evm().await;

    let result = pay(&f, &f.agent, RECIPIENT, VALUE).await;
    assert!(result.is_success(), "{result:?}");

    // The viem-built signed transaction, once.
    let events = events(&result);
    assert_eq!(events.len(), 1, "exactly one signed-payment event");
    let data = &events[0]["data"][0];
    assert_eq!(data["raw_tx"], raw_tx(HASH_NONCE_0));
    assert_eq!(data["nonce"], 0);
    assert_eq!(data["to"], RECIPIENT);
    assert_eq!(data["value"], VALUE.to_string());
    assert_eq!(data["chain_id"], CHAIN_ID);

    // Spend committed exactly once, at the outlay; reservation gone.
    assert_eq!(f.reserved(evm()).await, 0);
    assert_eq!(f.live_spend(evm()).await, OUTLAY);
    assert_eq!(nonce(&f).await, "1");

    // Nothing else was committed anywhere.
    assert_eq!(f.live_spend(near()).await, 0);
    assert_eq!(signer_requests(&signer).await.len(), 1);
}

#[tokio::test]
async fn the_signer_is_sent_the_hash_the_contract_computed() {
    let (f, signer) = armed_evm().await;
    pay(&f, &f.agent, RECIPIENT, VALUE)
        .await
        .into_result()
        .unwrap();
    pay(&f, &f.agent, RECIPIENT, VALUE)
        .await
        .into_result()
        .unwrap();

    let requests = signer_requests(&signer).await;
    assert_eq!(requests.len(), 2);
    // viem's keccak of viem's serialization of the owner-configured fields and
    // the contract-assigned nonce: the agent supplied only recipient and value.
    assert_eq!(requests[0]["hash"], HASH_NONCE_0);
    assert_eq!(requests[1]["hash"], HASH_NONCE_1);
    // The signer is asked as this contract, for the owner-configured key.
    assert_eq!(requests[0]["predecessor"], f.contract.id().to_string());
    assert_eq!(requests[0]["path"], PATH);
    assert_eq!(requests[0]["domain_id"], 0);
}

// ---- Failures release cleanly ----

#[tokio::test]
async fn a_failing_signer_releases_the_reservation_and_settles_nothing() {
    let (f, signer) = armed_evm().await;
    f.record_spend(evm(), 5).await;
    let before = (f.live_spend(evm()).await, dry_run_evm(&f, VALUE).await);
    set_mode(&signer, "Fail").await;

    let result = pay(&f, &f.agent, RECIPIENT, VALUE).await;
    assert!(
        result.is_success(),
        "the request itself succeeds: {result:?}"
    );

    assert!(events(&result).is_empty());
    assert!(logs_contain(&result, "not signed"));
    assert_eq!(f.reserved(evm()).await, 0);
    assert_eq!(
        (f.live_spend(evm()).await, dry_run_evm(&f, VALUE).await),
        before
    );
    // The nonce stays consumed: the documented outbound-only gap. The next
    // payment, once the signer recovers, uses nonce 1.
    assert_eq!(nonce(&f).await, "1");
    set_mode(&signer, "Sign").await;
    let next = pay(&f, &f.agent, RECIPIENT, VALUE).await;
    assert_eq!(events(&next)[0]["data"][0]["raw_tx"], raw_tx(HASH_NONCE_1));
}

/// G1: the signer's receipt succeeded, so whatever it returned is public and
/// possibly broadcastable. The outlay must be booked even though the contract
/// cannot turn the output into a transaction, and nothing must be emitted.
async fn assert_booked_but_not_emitted(mode: &str, expected_log: &str) {
    let (f, signer) = armed_evm().await;
    set_mode(&signer, mode).await;

    let result = pay(&f, &f.agent, RECIPIENT, VALUE).await;
    assert!(result.is_success(), "{mode}: {result:?}");

    assert!(events(&result).is_empty(), "{mode}: nothing is emitted");
    assert!(
        logs_contain(&result, "booked, no transaction emitted"),
        "{mode}"
    );
    assert!(
        logs_contain(&result, expected_log),
        "{mode}: {expected_log}"
    );
    assert_eq!(f.reserved(evm()).await, 0, "{mode}: released");
    assert_eq!(f.live_spend(evm()).await, OUTLAY, "{mode}: booked");
    assert_eq!(nonce(&f).await, "1", "{mode}: the nonce stays consumed");
    // The booked spend is real budget: it shows up in what is left.
    assert_eq!(f.live_spend(near()).await, 0);
}

#[tokio::test]
async fn a_signature_that_cannot_sign_a_transaction_is_booked_but_not_emitted() {
    // Recovery id 2: well-formed, rejected by try_assemble_signed.
    assert_booked_but_not_emitted("BadSignature", "signature rejected").await;
}

#[tokio::test]
async fn valid_json_of_the_wrong_shape_is_booked_but_not_emitted() {
    assert_booked_but_not_emitted("WrongShape", "signature rejected").await;
}

#[tokio::test]
async fn a_non_json_result_is_booked_but_not_emitted() {
    assert_booked_but_not_emitted("Garbage", "signature rejected").await;
}

#[tokio::test]
async fn a_genuine_signature_beyond_the_read_buffer_is_booked_but_not_emitted() {
    // The clearest G1 case: a real, assemblable signature the contract cannot
    // read because it exceeds the 4096-byte buffer.
    assert_booked_but_not_emitted("Oversized", "unreadable signer result").await;
}

#[tokio::test]
async fn a_genuine_signature_by_another_key_is_booked_but_not_emitted() {
    // The signature assembles and is genuine for the hash the contract sent,
    // but it recovers to a different address than the configured sender.
    assert_booked_but_not_emitted("OtherSigner", "authenticity failure").await;
}

#[tokio::test]
async fn the_callback_has_gas_to_spare_for_the_authenticity_check() {
    let (f, _signer) = armed_evm().await;
    let result = pay(&f, &f.agent, RECIPIENT, VALUE).await;
    assert!(result.is_success(), "{result:?}");
    assert_eq!(events(&result).len(), 1);
    // The callback is the receipt for on_sign_complete; it includes ecrecover
    // and keccak. The reserve is 25 Tgas.
    let burnt = result
        .receipt_outcomes()
        .iter()
        .find(|outcome| {
            outcome
                .logs
                .iter()
                .any(|line| line.starts_with("EVENT_JSON:"))
        })
        .map(|outcome| outcome.gas_burnt.as_gas())
        .expect("the callback receipt emitted the event");
    println!("on_sign_complete gas burnt: {burnt}");
    assert!(
        burnt < 12_500_000_000_000,
        "under half the 25 Tgas reserve: {burnt}"
    );
}

// ---- TOCTOU ----

#[tokio::test]
async fn a_second_request_sees_the_first_ones_reservation_before_it_settles() {
    let (f, signer) = armed_evm().await;
    signer
        .call("set_slow_hops")
        .args_json(json!({ "hops": 6 }))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();

    let first = f
        .agent
        .call(f.contract.id(), "request_evm_payment")
        .args_json(json!({ "recipient": RECIPIENT, "amount": VALUE.to_string() }))
        .gas(Gas::from_tgas(AGENT_TGAS))
        .transact_async()
        .await
        .unwrap();

    // The reservation is released only by the callback, so seeing it means the
    // signature has not come back.
    let mut seen = false;
    for _ in 0..600 {
        if f.reserved(evm()).await == OUTLAY {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen, "never observed the in-flight reservation");
    assert_eq!(
        f.live_spend(evm()).await,
        0,
        "not committed before the signature"
    );
    assert_eq!(nonce(&f).await, "1", "the nonce is taken at request time");

    // 5e13 alone fits the cap, but with the first in flight it exceeds the limit.
    let big = 50_000_000_000_000;
    assert_eq!(
        dry_run_evm(&f, big).await,
        Decision::Deny(DenyReason::WindowLimitExceeded)
    );
    let denied = pay(&f, &f.agent, RECIPIENT, big).await;
    assert_reverts(denied, "Denied: WindowLimitExceeded");
    assert_eq!(nonce(&f).await, "1", "a denied request takes no nonce");

    // A smaller second payment is allowed, and gets the next nonce.
    let second = pay(&f, &f.agent, RECIPIENT, VALUE).await;
    assert!(second.is_success(), "{second:?}");
    let first = first.await.unwrap();
    assert!(first.is_success(), "{first:?}");

    assert_eq!(events(&first)[0]["data"][0]["raw_tx"], raw_tx(HASH_NONCE_0));
    assert_eq!(
        events(&second)[0]["data"][0]["raw_tx"],
        raw_tx(HASH_NONCE_1)
    );
    assert_eq!(f.reserved(evm()).await, 0);
    assert_eq!(f.live_spend(evm()).await, 2 * OUTLAY);
    // Settlement moved the budget from reserved to committed, so the big
    // payment is still over the limit.
    assert_eq!(
        dry_run_evm(&f, big).await,
        Decision::Deny(DenyReason::WindowLimitExceeded)
    );
}

// ---- Denials ----

#[tokio::test]
async fn denied_requests_change_nothing_and_never_reach_the_signer() {
    let (f, signer) = armed_evm().await;

    assert_reverts(
        pay(&f, &f.outsider, RECIPIENT, VALUE).await,
        "Denied: NotAuthorized",
    );
    assert_reverts(
        pay(
            &f,
            &f.agent,
            "0x2222222222222222222222222222222222222222",
            VALUE,
        )
        .await,
        "Denied: RecipientNotAllowed",
    );
    // Within the cap by value alone, over it once the fee is counted.
    let over_with_fee = EVM_PER_TX_CAP - FEE_COST + 1;
    assert_reverts(
        pay(&f, &f.agent, RECIPIENT, over_with_fee).await,
        "Denied: PerTxCapExceeded",
    );
    f.owner_ok("freeze", json!({})).await;
    assert_reverts(pay(&f, &f.agent, RECIPIENT, VALUE).await, "Denied: Frozen");

    assert_eq!(f.reserved(evm()).await, 0);
    assert_eq!(f.live_spend(evm()).await, 0);
    assert_eq!(nonce(&f).await, "0");
    assert!(signer_requests(&signer).await.is_empty());
}

#[tokio::test]
async fn a_request_without_gas_for_the_callback_reverts_and_reserves_nothing() {
    let (f, signer) = armed_evm().await;
    let result = f
        .agent
        .call(f.contract.id(), "request_evm_payment")
        .args_json(json!({ "recipient": RECIPIENT, "amount": VALUE.to_string() }))
        .gas(Gas::from_tgas(40))
        .transact()
        .await
        .unwrap();
    assert!(result.is_failure());

    assert_eq!(f.reserved(evm()).await, 0);
    assert_eq!(nonce(&f).await, "0", "the nonce is not consumed either");
    assert!(signer_requests(&signer).await.is_empty());
}

#[tokio::test]
async fn evm_payments_need_configuration_and_a_valid_address() {
    let f = Fixture::armed().await;
    assert_reverts(pay(&f, &f.agent, RECIPIENT, VALUE).await, "not configured");

    let (f, _signer) = armed_evm().await;
    assert_reverts(
        pay(&f, &f.agent, "0x1234", VALUE).await,
        "Invalid EVM address",
    );
}

// ---- Owner configuration ----

#[tokio::test]
async fn evm_configuration_is_owner_only() {
    let (f, signer) = armed_evm().await;
    let calls = [
        (
            "set_evm_config",
            json!({
                "signer_id": signer.id(), "derivation_path": PATH, "sender": fixture_sender(),
                "chain_id": CHAIN_ID, "max_fee_per_gas": "1", "max_priority_fee_per_gas": "1", "gas_limit": null,
            }),
        ),
        (
            "set_evm_recipient",
            json!({ "address": RECIPIENT, "allowed": false }),
        ),
        (
            "raise_evm_nonce",
            json!({ "chain_id": CHAIN_ID, "nonce": "9" }),
        ),
    ];
    for who in [&f.outsider] {
        for (method, args) in &calls {
            assert_reverts(
                f.call(who, method, args.clone()).await,
                "Only the owner can call this method",
            );
        }
    }
    assert_eq!(nonce(&f).await, "0");
}

#[tokio::test]
async fn the_signing_identity_is_fixed_but_fees_can_change() {
    let (f, signer) = armed_evm().await;
    let config = |signer_id: &AccountId, path: &str, fee: &str| {
        json!({
            "signer_id": signer_id, "derivation_path": path, "sender": fixture_sender(),
            "chain_id": CHAIN_ID, "max_fee_per_gas": fee, "max_priority_fee_per_gas": "1", "gas_limit": 50000,
        })
    };
    f.owner_ok("set_evm_config", config(signer.id(), PATH, "3000000000"))
        .await;
    let stored: Value = f.view("get_evm_config", json!({})).await;
    assert_eq!(stored["max_fee_per_gas"], "3000000000");
    assert_eq!(stored["gas_limit"], 50000);

    assert_reverts(
        f.call(
            &f.owner,
            "set_evm_config",
            config(f.outsider.id(), PATH, "1"),
        )
        .await,
        "cannot change once set",
    );
    assert_reverts(
        f.call(
            &f.owner,
            "set_evm_config",
            config(signer.id(), "other-path", "1"),
        )
        .await,
        "cannot change once set",
    );
    let mut other_sender = config(signer.id(), PATH, "1");
    other_sender["sender"] = json!(RECIPIENT);
    assert_reverts(
        f.call(&f.owner, "set_evm_config", other_sender).await,
        "cannot change once set",
    );
    assert_eq!(stored["sender"], fixture_sender());
}

#[tokio::test]
async fn the_nonce_can_be_raised_but_never_lowered() {
    let (f, _signer) = armed_evm().await;
    f.owner_ok(
        "raise_evm_nonce",
        json!({ "chain_id": CHAIN_ID, "nonce": "5" }),
    )
    .await;
    assert_eq!(nonce(&f).await, "5");
    assert_reverts(
        f.call(
            &f.owner,
            "raise_evm_nonce",
            json!({ "chain_id": CHAIN_ID, "nonce": "4" }),
        )
        .await,
        "can only be raised",
    );
    let result = pay(&f, &f.agent, RECIPIENT, VALUE).await;
    assert_eq!(events(&result)[0]["data"][0]["nonce"], 5);
    assert_eq!(
        events(&result)[0]["data"][0]["raw_tx"],
        raw_tx(HASH_NONCE_5)
    );
    assert_eq!(nonce(&f).await, "6");
}

// ---- Boundaries between the NEAR and EVM paths ----

#[tokio::test]
async fn the_near_entry_points_refuse_evm_assets() {
    let (f, _signer) = armed_evm().await;
    assert_reverts(
        f.request_transfer(evm(), 1, f.recipient.id()).await,
        "request_evm_payment",
    );
    assert_reverts(
        f.call(
            &f.owner,
            "owner_withdraw",
            json!({ "asset": evm(), "amount": "1", "to": f.recipient.id() }),
        )
        .await,
        "request_evm_payment",
    );
}

#[tokio::test]
async fn only_the_contract_itself_can_call_the_signing_callback() {
    let (f, _signer) = armed_evm().await;
    for who in [&f.owner, &f.outsider] {
        let result = who
            .call(f.contract.id(), "on_sign_complete")
            .args(Vec::new())
            .transact()
            .await
            .unwrap();
        assert_reverts(result, "Method on_sign_complete is private");
    }
}
