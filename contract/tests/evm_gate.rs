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
// viem's signing hashes for the owner's withdrawals (see `vectors/gen-evm-signatures.mjs`):
// to an address no policy allows, far over every cap, at nonce 3 ...
const HASH_OWNER_OVER_CAP: &str =
    "d251a2d9e07335a0679670f35f0e4c7266f09985e33960e4bfd6895519cc2506";
// ... a zero-value transfer to the sender itself at nonce 0 (a cancellation) ...
const HASH_OWNER_CANCEL: &str = "7760e5f1f2353e980ecdf78f8821709fdc3347262f5dd2f8112a611ab05fcdfb";
// ... 1000 wei to the unallowed address at nonce 5, and the agent's payment at nonce 6.
const HASH_OWNER_NONCE_5: &str = "8775d42074fe48ab63c545d123b85ad7d9eabbde1ce276be49b9414658f6f8b8";
const HASH_AGENT_NONCE_6: &str = "8d45cd7aa9407c4649ac3163ca9519d674448dca44bd2efe0b38aa50f3085761";
// viem's signing hashes for the agent's ERC-20 transfers of USDC (Sepolia) to
// RECIPIENT: 1000 units at nonce 0 and 2000 at nonce 1, at the default token gas
// limit of 100000. The calldata is viem's ABI encoding.
const HASH_TOKEN_NONCE_0: &str = "457bff992d74e59ac0dc6b9fcae10b15efa6272f2a9674ef1fa4193a23631b85";
const HASH_TOKEN_NONCE_1: &str = "3c15cb8ec54c0e0f027648d8a88cd17d53e5366b6ce2a2281fc58a9a82869b0b";
const TOKEN: &str = "0x1c7d4b196cb0c7b01d743fbc6116a902379c7238";
// viem's signing hashes for the owner's token recoveries: 1e30 tokens (far over
// any cap) to an address no policy allows at nonce 3; zero tokens to the sender
// itself at nonce 0 (a cancellation); 1000 to the unallowed address at nonce 5;
// and the agent's token payment of 1000 at nonce 4, the nonce after the first.
const HASH_OWNER_TOKEN_OVER_CAP: &str =
    "1ac5ca85c8eaafee245b26a52c30590065696e4be08f6c94b1e1c7daa8df9432";
const HASH_OWNER_TOKEN_CANCEL: &str =
    "659f396f4e4928def1efa7561ee1242dbcf4e1fa67b26efb4aee4f9006e5834f";
const HASH_OWNER_TOKEN_NONCE_5: &str =
    "6498e5f07e1a5e747d07090dad2b65091247fe05a6c63c0f07d1e452ef4e19ea";
const HASH_AGENT_TOKEN_NONCE_4: &str =
    "dbd232000f859688284e6274b0dea8c7703f8370c47d5861c19d996fe5c05dc6";
/// 1e30: far over any token cap.
const HUGE_TOKENS: u128 = 1_000_000_000_000_000_000_000_000_000_000;
/// 100000 gas at 2 gwei: what a token payment's gas is accounted at.
const TOKEN_GAS: u128 = 200_000_000_000_000;
const NOT_ALLOWLISTED: &str = "0x00000000000000000000000000000000000000aa";
/// Far over the per-transaction cap and the window limit.
const HUGE: u128 = 5_000_000_000_000_000;
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
    armed_evm_with_sender(&fixture_sender()).await
}

/// As `armed_evm`, with `sender` configured as the contract's EVM address.
async fn armed_evm_with_sender(sender: &str) -> (Fixture, Contract) {
    let f = Fixture::armed().await;
    let signer = f.deploy_signer().await;
    f.register_signatures(
        &signer,
        &[
            HASH_NONCE_0,
            HASH_NONCE_1,
            HASH_NONCE_5,
            HASH_OWNER_OVER_CAP,
            HASH_OWNER_CANCEL,
            HASH_OWNER_NONCE_5,
            HASH_AGENT_NONCE_6,
            HASH_TOKEN_NONCE_0,
            HASH_TOKEN_NONCE_1,
            HASH_OWNER_TOKEN_OVER_CAP,
            HASH_OWNER_TOKEN_CANCEL,
            HASH_OWNER_TOKEN_NONCE_5,
            HASH_AGENT_TOKEN_NONCE_4,
        ],
    )
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
            "sender": sender,
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

// ---- Owner recovery: owner_evm_withdraw ----

async fn owner_withdraw(
    f: &Fixture,
    recipient: &str,
    value: u128,
    nonce: u64,
) -> ExecutionFinalResult {
    owner_withdraw_as(f, &f.owner, recipient, value, nonce).await
}

/// The event an owner withdrawal emits.
fn owner_events(result: &ExecutionFinalResult) -> Vec<Value> {
    let events = events(result);
    for event in &events {
        assert_eq!(event["event"], "evm_owner_withdrawal_signed");
    }
    events
}

#[tokio::test]
async fn only_the_owner_can_withdraw_cross_chain() {
    let (f, signer) = armed_evm().await;
    // A stranger reaches the contract and is refused by the owner check. The
    // agent's own key does not even get that far: the protocol refuses it
    // (see tests/agent_key.rs, which tries every method).
    assert_reverts(
        owner_withdraw_as(&f, &f.outsider, RECIPIENT, 1, 0).await,
        "Only the owner can call this method",
    );
    assert!(signer_requests(&signer).await.is_empty());
    assert_eq!(nonce(&f).await, "0");
}

/// Like the agent's request it dispatches the signer (30 Tgas) and reserves
/// 25 Tgas for the callback, so it needs the gas for both on top of its own.
async fn owner_withdraw_as(
    f: &Fixture,
    who: &near_workspaces::Account,
    recipient: &str,
    value: u128,
    nonce: u64,
) -> ExecutionFinalResult {
    who.call(f.contract.id(), "owner_evm_withdraw")
        .args_json(json!({
            "recipient": recipient, "value": value.to_string(), "nonce": nonce.to_string(),
        }))
        .gas(Gas::from_tgas(100))
        .transact()
        .await
        .unwrap()
}

#[tokio::test]
async fn the_owner_can_withdraw_while_frozen_outside_policy_with_no_accounting() {
    let (f, signer) = armed_evm().await;
    f.record_spend(evm(), 5).await;
    f.owner_ok("freeze", json!({})).await;
    assert_reverts(pay(&f, &f.agent, RECIPIENT, VALUE).await, "Denied: Frozen");
    let before = (f.reserved(evm()).await, f.live_spend(evm()).await);

    // Far over every cap, to an address on no allowlist, while frozen.
    const { assert!(HUGE > EVM_PER_TX_CAP && HUGE > EVM_WINDOW_LIMIT) };
    let result = owner_withdraw(&f, NOT_ALLOWLISTED, HUGE, 3).await;
    assert!(result.is_success(), "{result:?}");

    // viem's hash and viem's signed transaction, emitted once.
    let requests = signer_requests(&signer).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["hash"], HASH_OWNER_OVER_CAP);
    assert_eq!(requests[0]["predecessor"], f.contract.id().to_string());
    let events = owner_events(&result);
    assert_eq!(events.len(), 1);
    let data = &events[0]["data"][0];
    assert_eq!(data["raw_tx"], raw_tx(HASH_OWNER_OVER_CAP));
    assert_eq!(data["nonce"], 3);
    assert_eq!(data["to"], NOT_ALLOWLISTED);
    assert_eq!(data["value"], HUGE.to_string());

    // No reservation, no booking.
    assert_eq!((f.reserved(evm()).await, f.live_spend(evm()).await), before);
    // The explicit nonce moved the counter past itself.
    assert_eq!(nonce(&f).await, "4");
}

#[tokio::test]
async fn the_counter_follows_max_of_counter_and_nonce_plus_one_and_the_agent_never_collides() {
    let (f, signer) = armed_evm().await;
    // The agent's transaction at nonce 0.
    let first = pay(&f, &f.agent, RECIPIENT, VALUE).await;
    assert!(first.is_success(), "{first:?}");
    assert_eq!(nonce(&f).await, "1");

    // A cancellation of it: the same nonce, so the counter is untouched.
    let cancel = owner_withdraw(&f, &fixture_sender(), 0, 0).await;
    assert!(cancel.is_success(), "{cancel:?}");
    assert_eq!(
        owner_events(&cancel)[0]["data"][0]["raw_tx"],
        raw_tx(HASH_OWNER_CANCEL)
    );
    assert_eq!(nonce(&f).await, "1", "a replacement leaves the counter");

    // A future nonce advances it past that nonce.
    let ahead = owner_withdraw(&f, NOT_ALLOWLISTED, 1000, 5).await;
    assert_eq!(
        owner_events(&ahead)[0]["data"][0]["raw_tx"],
        raw_tx(HASH_OWNER_NONCE_5)
    );
    assert_eq!(nonce(&f).await, "6");

    // The agent's next transaction takes nonce 6, never one the owner used.
    let next = pay(&f, &f.agent, RECIPIENT, VALUE).await;
    assert!(next.is_success(), "{next:?}");
    let requests = signer_requests(&signer).await;
    assert_eq!(requests.last().unwrap()["hash"], HASH_AGENT_NONCE_6);
    assert_eq!(
        events(&next)[0]["data"][0]["raw_tx"],
        raw_tx(HASH_AGENT_NONCE_6)
    );
    assert_eq!(nonce(&f).await, "7");
}

#[tokio::test]
async fn a_signature_by_another_key_is_not_emitted_for_the_owner() {
    let (f, signer) = armed_evm().await;
    set_mode(&signer, "OtherSigner").await;
    let result = owner_withdraw(&f, NOT_ALLOWLISTED, HUGE, 3).await;
    assert!(result.is_success(), "{result:?}");
    assert!(events(&result).is_empty(), "nothing is emitted");
    assert!(logs_contain(
        &result,
        "owner evm withdrawal not emitted: authenticity failure"
    ));
    assert!(logs_contain(&result, "signer or configuration fault"));
    assert_eq!(f.reserved(evm()).await, 0);
    assert_eq!(f.live_spend(evm()).await, 0, "the owner path books nothing");
}

#[tokio::test]
async fn a_mismatched_sender_configuration_emits_nothing_for_the_owner() {
    let (f, _signer) = armed_evm_with_sender(RECIPIENT).await;
    // A cancellation at nonce 0, which leaves the agent nonce 1 (a registered one).
    let result = owner_withdraw(&f, &fixture_sender(), 0, 0).await;
    assert!(result.is_success(), "{result:?}");
    assert!(events(&result).is_empty());
    assert!(logs_contain(&result, "authenticity failure"));
    // The agent path agrees: the same configuration emits nothing for it either.
    let payment = pay(&f, &f.agent, RECIPIENT, VALUE).await;
    assert!(events(&payment).is_empty());
    assert!(logs_contain(&payment, "authenticity failure"));
}

#[tokio::test]
async fn a_failed_signer_call_changes_nothing_for_the_owner() {
    let (f, signer) = armed_evm().await;
    set_mode(&signer, "Fail").await;
    let result = owner_withdraw(&f, NOT_ALLOWLISTED, HUGE, 3).await;
    assert!(result.is_success(), "{result:?}");
    assert!(events(&result).is_empty());
    assert!(logs_contain(&result, "owner evm withdrawal not signed"));
    assert_eq!((f.reserved(evm()).await, f.live_spend(evm()).await), (0, 0));
}

#[tokio::test]
async fn the_owner_callback_has_gas_to_spare() {
    let (f, _signer) = armed_evm().await;
    let result = owner_withdraw(&f, NOT_ALLOWLISTED, HUGE, 3).await;
    assert_eq!(owner_events(&result).len(), 1);
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
    println!("on_owner_sign_complete gas burnt: {burnt}");
    assert!(
        burnt < 12_500_000_000_000,
        "under half the 25 Tgas reserve: {burnt}"
    );
}

// ---- ERC-20 token payments ----

/// The token as the `AssetId` JSON takes it: the chain and the token's 20 bytes.
fn token_asset() -> Value {
    let bytes: Vec<u8> = (0..20)
        .map(|i| u8::from_str_radix(&TOKEN[2 + 2 * i..4 + 2 * i], 16).unwrap())
        .collect();
    json!({ "Evm20": { "chain_id": CHAIN_ID, "token": bytes } })
}

/// `armed_evm`, with room in the native policy for the gas of two token
/// transfers (each 2e14) and a policy for the token itself.
async fn armed_token() -> (Fixture, Contract) {
    let (f, signer) = armed_evm().await;
    f.set_policy(evm(), 400_000_000_000_000, 500_000_000_000_000)
        .await;
    f.set_policy(token_asset(), 5_000, 8_000).await;
    (f, signer)
}

async fn pay_token(
    f: &Fixture,
    who: &near_workspaces::Account,
    recipient: &str,
    amount: u128,
) -> ExecutionFinalResult {
    who.call(f.contract.id(), "request_evm_token_payment")
        .args_json(json!({ "token": TOKEN, "recipient": recipient, "amount": amount.to_string() }))
        .gas(Gas::from_tgas(AGENT_TGAS))
        .transact()
        .await
        .unwrap()
}

#[tokio::test]
async fn the_agent_key_pays_a_token_the_contract_builds_and_both_assets_are_accounted() {
    let (f, signer) = armed_token().await;
    let result = pay_token(&f, &f.agent, RECIPIENT, 1000).await;
    assert!(result.is_success(), "{result:?}");

    // The signer was sent viem's hash of the transfer: to the token, no ETH, the
    // calldata viem ABI-encodes, at the token gas limit.
    let requests = signer_requests(&signer).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["hash"], HASH_TOKEN_NONCE_0);
    // And the emitted transaction is viem's serialization of it, signed by the sender.
    let events = events(&result);
    assert_eq!(events.len(), 1);
    let data = &events[0]["data"][0];
    assert_eq!(data["raw_tx"], raw_tx(HASH_TOKEN_NONCE_0));
    assert_eq!(data["to"], TOKEN);
    assert_eq!(data["value"], "0");

    // Dual-asset accounting: the amount against the token, the gas against ETH.
    assert_eq!(f.reserved(token_asset()).await, 0);
    assert_eq!(f.reserved(evm()).await, 0);
    assert_eq!(f.live_spend(token_asset()).await, 1000);
    assert_eq!(f.live_spend(evm()).await, TOKEN_GAS);
    assert_eq!(nonce(&f).await, "1");
}

#[tokio::test]
async fn an_in_flight_token_payment_holds_both_reservations_and_releases_both_when_it_fails() {
    let (f, signer) = armed_token().await;
    // The native window holds the gas of one token transfer (2e14), not two.
    f.set_policy(evm(), 400_000_000_000_000, 300_000_000_000_000)
        .await;
    signer
        .call("set_slow_hops")
        .args_json(json!({ "hops": 6 }))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    // 5000 at nonce 0 has no signature registered with the mock signer, so it
    // fails when it comes back: its reservations must be released, not booked.
    let first = f
        .agent
        .call(f.contract.id(), "request_evm_token_payment")
        .args_json(json!({ "token": TOKEN, "recipient": RECIPIENT, "amount": "5000" }))
        .gas(Gas::from_tgas(AGENT_TGAS))
        .transact_async()
        .await
        .unwrap();

    let mut seen = false;
    for _ in 0..600 {
        if f.reserved(token_asset()).await == 5000 {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen, "never observed the in-flight reservation");
    // Both legs are held while the signature is outstanding, nothing committed.
    assert_eq!(f.reserved(evm()).await, TOKEN_GAS);
    assert_eq!(f.live_spend(token_asset()).await, 0);
    assert_eq!(f.live_spend(evm()).await, 0);

    // The token window (8000) is counted with the 5000 in flight ...
    assert_reverts(
        pay_token(&f, &f.agent, RECIPIENT, 4000).await,
        "Denied: WindowLimitExceeded (token transfer)",
    );
    // ... and so is the gas: the token side would allow 100, the native window
    // cannot hold a second transfer's gas.
    assert_reverts(
        pay_token(&f, &f.agent, RECIPIENT, 100).await,
        "Denied: WindowLimitExceeded (gas, native ETH)",
    );
    assert_eq!(nonce(&f).await, "1", "denied requests take no nonce");

    let first = first.await.unwrap();
    assert!(first.is_success(), "{first:?}");
    assert!(logs_contain(&first, "not signed"));
    // The failed receipt released both reservations and booked neither ...
    assert_eq!(f.reserved(token_asset()).await, 0);
    assert_eq!(f.reserved(evm()).await, 0);
    assert_eq!(f.live_spend(token_asset()).await, 0);
    assert_eq!(f.live_spend(evm()).await, 0);
    // ... so the budget it held is available again: 2000 at nonce 1 is signed.
    set_mode(&signer, "Sign").await;
    let second = pay_token(&f, &f.agent, RECIPIENT, 2000).await;
    assert!(second.is_success(), "{second:?}");
    assert_eq!(
        events(&second)[0]["data"][0]["raw_tx"],
        raw_tx(HASH_TOKEN_NONCE_1)
    );
    assert_eq!(f.live_spend(token_asset()).await, 2000);
    assert_eq!(f.live_spend(evm()).await, TOKEN_GAS);
}

#[tokio::test]
async fn token_denials_name_the_leg_and_change_nothing() {
    let (f, signer) = armed_token().await;
    assert_reverts(
        pay_token(&f, &f.agent, RECIPIENT, 5001).await,
        "Denied: PerTxCapExceeded (token transfer)",
    );
    assert_reverts(
        pay_token(&f, &f.agent, NOT_ALLOWLISTED, 100).await,
        "Denied: RecipientNotAllowed (token transfer)",
    );
    // Gas above the native per-transaction cap.
    f.set_policy(evm(), TOKEN_GAS - 1, 10 * TOKEN_GAS).await;
    assert_reverts(
        pay_token(&f, &f.agent, RECIPIENT, 100).await,
        "Denied: PerTxCapExceeded (gas, native ETH)",
    );
    // And with no native policy at all, a clear message.
    f.owner_ok("remove_asset_policy", json!({ "asset": evm() }))
        .await;
    assert_reverts(
        pay_token(&f, &f.agent, RECIPIENT, 100).await,
        "needs a native ETH policy for its gas",
    );
    // The view says the same, leg by leg.
    let view: Value = f
        .view(
            "dry_run_evm_token",
            json!({ "agent_key": f.agent_key, "token": TOKEN, "recipient": RECIPIENT, "amount": "100" }),
        )
        .await;
    assert_eq!(view["token"], "Allow");
    assert_eq!(view["gas"], json!({ "Deny": "AssetNotAllowed" }));

    assert!(signer_requests(&signer).await.is_empty());
    assert_eq!(nonce(&f).await, "0");
    assert_eq!(f.reserved(token_asset()).await, 0);
}

#[tokio::test]
async fn a_failing_signer_releases_both_legs_and_books_neither() {
    let (f, signer) = armed_token().await;
    set_mode(&signer, "Fail").await;
    let result = pay_token(&f, &f.agent, RECIPIENT, 1000).await;
    assert!(result.is_success(), "{result:?}");
    assert!(events(&result).is_empty());
    assert!(logs_contain(&result, "not signed"));
    assert_eq!(f.reserved(token_asset()).await, 0);
    assert_eq!(f.reserved(evm()).await, 0);
    assert_eq!(f.live_spend(token_asset()).await, 0);
    assert_eq!(f.live_spend(evm()).await, 0);

    // A signature by another key is booked on both legs (the receipt succeeded)
    // but not emitted.
    set_mode(&signer, "OtherSigner").await;
    let result = pay_token(&f, &f.agent, RECIPIENT, 2000).await;
    assert!(events(&result).is_empty());
    assert!(logs_contain(&result, "authenticity failure"));
    assert_eq!(f.live_spend(token_asset()).await, 2000);
    assert_eq!(f.live_spend(evm()).await, TOKEN_GAS);
}

#[tokio::test]
async fn the_token_callback_has_gas_to_spare() {
    let (f, _signer) = armed_token().await;
    let result = pay_token(&f, &f.agent, RECIPIENT, 1000).await;
    assert_eq!(events(&result).len(), 1);
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
    println!("token on_sign_complete gas burnt: {burnt}");
    assert!(
        burnt < 12_500_000_000_000,
        "under half the 25 Tgas reserve: {burnt}"
    );
}

// ---- Owner recovery of tokens: owner_evm_token_withdraw ----

async fn owner_token_withdraw(
    f: &Fixture,
    who: &near_workspaces::Account,
    recipient: &str,
    amount: u128,
    nonce: u64,
) -> ExecutionFinalResult {
    who.call(f.contract.id(), "owner_evm_token_withdraw")
        .args_json(json!({
            "token": TOKEN, "recipient": recipient, "amount": amount.to_string(),
            "nonce": nonce.to_string(),
        }))
        .gas(Gas::from_tgas(100))
        .transact()
        .await
        .unwrap()
}

#[tokio::test]
async fn only_the_owner_can_recover_tokens_and_the_agent_key_is_refused_by_the_protocol() {
    let (f, signer) = armed_token().await;
    assert_reverts(
        owner_token_withdraw(&f, &f.outsider, RECIPIENT, 1, 0).await,
        "Only the owner can call this method",
    );
    // The agent's key is limited to the agent methods: the runtime refuses the
    // call before any contract code runs.
    let refused = f
        .agent
        .call(f.contract.id(), "owner_evm_token_withdraw")
        .args_json(json!({ "token": TOKEN, "recipient": RECIPIENT, "amount": "1", "nonce": "0" }))
        .gas(Gas::from_tgas(100))
        .transact()
        .await;
    assert!(
        format!("{refused:?}").contains("MethodNameMismatch"),
        "{refused:?}"
    );
    assert!(signer_requests(&signer).await.is_empty());
    assert_eq!(nonce(&f).await, "0");
}

#[tokio::test]
async fn the_owner_recovers_tokens_while_frozen_outside_policy_with_no_accounting() {
    // No policy for the token, and the native policy too small for any token
    // payment's gas: the agent could not move a single token.
    let (f, signer) = armed_evm().await;
    f.owner_ok("freeze", json!({})).await;
    let before = (
        f.reserved(token_asset()).await,
        f.live_spend(token_asset()).await,
        f.reserved(evm()).await,
        f.live_spend(evm()).await,
    );

    let result = owner_token_withdraw(&f, &f.owner, NOT_ALLOWLISTED, HUGE_TOKENS, 3).await;
    assert!(result.is_success(), "{result:?}");

    // viem's hash of the transfer, and viem's signed serialization of it.
    let requests = signer_requests(&signer).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["hash"], HASH_OWNER_TOKEN_OVER_CAP);
    let events = owner_events(&result);
    assert_eq!(events.len(), 1);
    let data = &events[0]["data"][0];
    assert_eq!(data["raw_tx"], raw_tx(HASH_OWNER_TOKEN_OVER_CAP));
    assert_eq!(data["to"], TOKEN);
    assert_eq!(data["value"], "0");
    assert_eq!(data["nonce"], 3);

    assert_eq!(
        (
            f.reserved(token_asset()).await,
            f.live_spend(token_asset()).await,
            f.reserved(evm()).await,
            f.live_spend(evm()).await,
        ),
        before
    );
    assert_eq!(nonce(&f).await, "4");
}

#[tokio::test]
async fn a_token_cancellation_leaves_the_counter_and_a_future_nonce_moves_it_past_itself() {
    let (f, signer) = armed_token().await;
    // The agent's token payment takes nonce 0.
    let first = pay_token(&f, &f.agent, RECIPIENT, 1000).await;
    assert!(first.is_success(), "{first:?}");
    assert_eq!(nonce(&f).await, "1");

    // Cancelling it: the same nonce, zero tokens to the sender itself.
    let cancel = owner_token_withdraw(&f, &f.owner, &fixture_sender(), 0, 0).await;
    assert_eq!(
        owner_events(&cancel)[0]["data"][0]["raw_tx"],
        raw_tx(HASH_OWNER_TOKEN_CANCEL)
    );
    assert_eq!(nonce(&f).await, "1");

    // A future nonce moves the counter past it, and the agent then takes the
    // next free one.
    let ahead = owner_token_withdraw(&f, &f.owner, NOT_ALLOWLISTED, 1000, 3).await;
    assert!(ahead.is_success(), "{ahead:?}");
    assert_eq!(nonce(&f).await, "4");
    let next = pay_token(&f, &f.agent, RECIPIENT, 1000).await;
    assert!(next.is_success(), "{next:?}");
    let requests = signer_requests(&signer).await;
    assert_eq!(requests.last().unwrap()["hash"], HASH_AGENT_TOKEN_NONCE_4);
    assert_eq!(
        events(&next)[0]["data"][0]["raw_tx"],
        raw_tx(HASH_AGENT_TOKEN_NONCE_4)
    );
    assert_eq!(nonce(&f).await, "5");
}

#[tokio::test]
async fn a_token_recovery_signed_by_another_key_or_a_mismatched_sender_is_not_emitted() {
    let (f, signer) = armed_evm().await;
    set_mode(&signer, "OtherSigner").await;
    let result = owner_token_withdraw(&f, &f.owner, NOT_ALLOWLISTED, HUGE_TOKENS, 3).await;
    assert!(result.is_success(), "{result:?}");
    assert!(events(&result).is_empty());
    assert!(logs_contain(
        &result,
        "owner evm withdrawal not emitted: authenticity failure"
    ));

    let (f, _signer) = armed_evm_with_sender(RECIPIENT).await;
    let result = owner_token_withdraw(&f, &f.owner, &fixture_sender(), 0, 0).await;
    assert!(result.is_success(), "{result:?}");
    assert!(events(&result).is_empty());
    assert!(logs_contain(&result, "authenticity failure"));
}

#[tokio::test]
async fn the_token_recovery_callback_has_gas_to_spare() {
    let (f, _signer) = armed_evm().await;
    let result = owner_token_withdraw(&f, &f.owner, NOT_ALLOWLISTED, HUGE_TOKENS, 3).await;
    assert_eq!(owner_events(&result).len(), 1);
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
    println!("on_owner_sign_complete (token) gas burnt: {burnt}");
    assert!(
        burnt < 12_500_000_000_000,
        "under half the 25 Tgas reserve: {burnt}"
    );
}
