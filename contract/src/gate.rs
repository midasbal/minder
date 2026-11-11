//! The cross-chain signing gate: the agent asks for an EVM payment, the
//! contract reserves budget, builds the transaction itself, asks the Chain
//! Signatures contract to sign its hash, and settles when the signature (or a
//! failure) comes back. It reuses the reserve/settle/release lifecycle of
//! `request_local_transfer`, with a `sign` call in place of the transfer.
//!
//! There is deliberately no method that signs a caller-supplied hash: the
//! only hash that ever reaches the signer is `evm::signing_hash` of a
//! transaction built here from owner-set fields and a contract-assigned nonce.

use near_sdk::json_types::{U128, U64};
use near_sdk::serde::Deserialize;
use near_sdk::serde_json::{self, json};
use near_sdk::{env, near, require, AccountId, Gas, NearToken, Promise, PromiseError};

use crate::evm::{self, Eip1559Tx, EvmError};
use crate::types::{AssetId, Decision, EvmConfig};
use crate::{page, Minder, MinderExt};

/// The signer requires at least 15 Tgas on `sign` and 1 yoctoNEAR.
const SIGN_GAS: Gas = Gas::from_tgas(30);
const SIGN_DEPOSIT: NearToken = NearToken::from_yoctonear(1);
/// Reserved up front for `on_sign_complete`, like the transfer callback: it
/// must always have the gas to release the reservation.
const SIGN_CALLBACK_GAS: Gas = Gas::from_tgas(25);
/// `domain_id` 0 is the signer's secp256k1 ECDSA key.
const SECP256K1_DOMAIN_ID: u64 = 0;
/// A signature response is a few hundred bytes of JSON; anything far larger
/// is not one.
const MAX_SIGNATURE_RESPONSE_BYTES: usize = 4096;
const DEFAULT_GAS_LIMIT: u64 = 21_000;

#[near]
impl Minder {
    // ---- Owner-only ----

    /// Allows or disallows an EVM recipient (`0x` + 40 hex digits).
    pub fn set_evm_recipient(&mut self, address: String, allowed: bool) {
        self.assert_owner();
        let address = parse_address(&address);
        if allowed {
            self.evm_recipients.insert(address);
        } else {
            self.evm_recipients.remove(&address);
        }
    }

    /// Sets how EVM payments are built. `signer_id` and `derivation_path`
    /// identify the signing key, so they are fixed once set: the nonce counter
    /// is only meaningful for one sending address. Chain id and fees may be
    /// changed. `gas_limit` defaults to 21000, a plain ETH transfer.
    pub fn set_evm_config(
        &mut self,
        signer_id: AccountId,
        derivation_path: String,
        chain_id: u64,
        max_fee_per_gas: U128,
        max_priority_fee_per_gas: U128,
        gas_limit: Option<u64>,
    ) {
        self.assert_owner();
        let config = EvmConfig {
            signer_id,
            derivation_path,
            chain_id,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            gas_limit: gas_limit.unwrap_or(DEFAULT_GAS_LIMIT),
        };
        require!(config.chain_id > 0, "chain_id must be positive");
        require!(config.gas_limit > 0, "gas_limit must be positive");
        require!(
            !config.derivation_path.is_empty(),
            "derivation_path must not be empty"
        );
        require!(
            config.max_priority_fee_per_gas.0 <= config.max_fee_per_gas.0,
            "max_priority_fee_per_gas cannot exceed max_fee_per_gas"
        );
        require!(
            config
                .max_fee_per_gas
                .0
                .checked_mul(u128::from(config.gas_limit))
                .is_some(),
            "max_fee_per_gas * gas_limit overflows"
        );
        if let Some(existing) = &self.evm_config {
            require!(
                existing.signer_id == config.signer_id
                    && existing.derivation_path == config.derivation_path,
                "signer_id and derivation_path cannot change once set"
            );
        }
        self.evm_config = Some(config);
    }

    /// Raises the next nonce for `chain_id` to at least `nonce`, for a sending
    /// address that already has history on that chain. It can only go up, so it
    /// can open a gap but can never reissue a nonce.
    pub fn raise_evm_nonce(&mut self, chain_id: u64, nonce: U64) {
        self.assert_owner();
        require!(
            nonce.0 >= self.next_nonce(chain_id),
            "The nonce can only be raised"
        );
        self.evm_nonces.insert(chain_id, nonce.0);
    }

    // ---- Agent ----

    /// Pays `amount` wei of native ETH to `recipient` on the configured chain,
    /// if policy allows, by getting the Chain Signatures contract to sign the
    /// transaction. Everything about the transaction except the recipient and
    /// value comes from owner configuration and the contract's own nonce, and
    /// the hash sent for signing is computed here.
    ///
    /// Policy counts the *outlay*, the value plus the worst-case fee
    /// (`gas_limit * max_fee_per_gas`), because fees leave the same address:
    /// otherwise many tiny payments would drain it through fees while each
    /// stayed under every cap. A denial reverts. On allow the outlay is
    /// reserved before any promise exists; `on_sign_complete` releases it.
    ///
    /// The nonce is taken now, so a signing that fails leaves a gap in the
    /// sender's nonces on the destination chain; later payments queue behind
    /// it until the owner fills it. That is accepted for this outbound-only
    /// design and not reclaimed here.
    pub fn request_evm_payment(&mut self, recipient: String, amount: U128) -> Promise {
        let config = self.require_evm_config();
        let to = parse_address(&recipient);
        let caller = env::predecessor_account_id();
        let asset = AssetId::Evm(config.chain_id);
        let outlay = config.outlay(amount.0);

        let recipient_allowed = self.evm_recipients.contains(&to);
        if let Decision::Deny(reason) =
            self.evaluate_with(&caller, &asset, outlay, recipient_allowed)
        {
            env::panic_str(&format!("Denied: {reason:?}"));
        }

        self.reserve(&asset, outlay);

        let nonce = self.take_nonce(config.chain_id);
        let tx = Eip1559Tx {
            chain_id: config.chain_id,
            nonce,
            max_priority_fee_per_gas: config.max_priority_fee_per_gas.0,
            max_fee_per_gas: config.max_fee_per_gas.0,
            gas_limit: config.gas_limit,
            to,
            value: amount.0,
        };
        let hash = evm::signing_hash(&tx);

        Promise::new(config.signer_id)
            .function_call(
                "sign".to_string(),
                json!({
                    "request": {
                        "path": config.derivation_path,
                        "payload_v2": { "Ecdsa": evm::hex_encode(&hash) },
                        "domain_id": SECP256K1_DOMAIN_ID,
                    }
                })
                .to_string()
                .into_bytes(),
                SIGN_DEPOSIT,
                SIGN_GAS,
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(SIGN_CALLBACK_GAS)
                    .on_sign_complete(asset, outlay, tx),
            )
    }

    /// Settles a payment started by `request_evm_payment`. Must never panic,
    /// or the reservation would be stranded.
    ///
    /// It releases the reservation first and unconditionally. Whether to book
    /// the spend then depends only on whether the signer's receipt *succeeded*,
    /// never on whether this contract could read, size-fit, parse or assemble
    /// what it returned:
    /// - **Receipt failed** (`PromiseError::Failed`, which includes the signer's
    ///   own timeout): nothing was signed. Nothing is booked.
    /// - **Receipt did not fail**: the signer produced output that sits in a
    ///   public receipt and may be broadcastable by anyone, whether or not we can
    ///   use it. The outlay is booked unconditionally. Only if the output then
    ///   assembles into a valid transaction is the raw signed transaction
    ///   emitted for an off-chain relayer; otherwise the spend stays booked, the
    ///   rejection is logged, and nothing is emitted.
    ///
    /// The arguments are borsh, not JSON: they are produced only by this
    /// contract, and the transaction carries `u128` fields.
    #[private]
    pub fn on_sign_complete(
        &mut self,
        #[serializer(borsh)] asset: AssetId,
        #[serializer(borsh)] outlay: u128,
        #[serializer(borsh)] tx: Eip1559Tx,
    ) {
        self.release(&asset, outlay);

        // Read bytes and parse them ourselves: `#[callback_result]` would
        // panic on a payload that does not deserialize. The size cap only
        // decides whether we can read the bytes; it says nothing about
        // whether the receipt succeeded.
        let response = match env::promise_result_checked(0, MAX_SIGNATURE_RESPONSE_BYTES) {
            Ok(bytes) => Some(bytes),
            // The only proof that nothing was signed.
            Err(PromiseError::Failed) => {
                env::log_str("evm payment not signed: the signer call failed");
                return;
            }
            // `TooLong`: the receipt succeeded but its output exceeds our read
            // buffer. Any variant added later (`PromiseError` is non_exhaustive)
            // is likewise not proof of failure.
            Err(_) => None,
        };

        // Fail-safe: a receipt that did not fail may hold a usable signature.
        self.record_spend(&asset, outlay);

        let Some(response) = response else {
            env::log_str("evm payment booked, no transaction emitted: unreadable signer result");
            return;
        };
        match assemble_from_response(&tx, &response) {
            Ok(raw_tx) => env::log_str(&signed_payment_event(&tx, &raw_tx)),
            Err(rejection) => env::log_str(&format!(
                "evm payment booked, no transaction emitted: signature rejected ({rejection:?})"
            )),
        }
    }

    // ---- Views ----

    /// `dry_run` for EVM payments: what `caller` would get from
    /// `request_evm_payment` right now, counting in-flight reservations.
    pub fn dry_run_evm(&self, caller: AccountId, recipient: String, amount: U128) -> Decision {
        let config = self.require_evm_config();
        let to = parse_address(&recipient);
        self.evaluate_with(
            &caller,
            &AssetId::Evm(config.chain_id),
            config.outlay(amount.0),
            self.evm_recipients.contains(&to),
        )
    }

    pub fn get_evm_config(&self) -> Option<EvmConfig> {
        self.evm_config.clone()
    }

    /// The nonce the next payment on `chain_id` will use.
    pub fn get_evm_nonce(&self, chain_id: u64) -> U64 {
        U64(self.next_nonce(chain_id))
    }

    pub fn is_evm_recipient_allowed(&self, address: String) -> bool {
        self.evm_recipients.contains(&parse_address(&address))
    }

    pub fn get_evm_recipients(&self, from_index: Option<u32>, limit: Option<u32>) -> Vec<String> {
        let (skip, take) = page(from_index, limit);
        self.evm_recipients
            .iter()
            .skip(skip)
            .take(take)
            .map(evm::format_address)
            .collect()
    }
}

impl Minder {
    fn require_evm_config(&self) -> EvmConfig {
        self.evm_config
            .clone()
            .unwrap_or_else(|| env::panic_str("EVM payments are not configured"))
    }

    fn next_nonce(&self, chain_id: u64) -> u64 {
        self.evm_nonces.get(&chain_id).copied().unwrap_or(0)
    }

    /// Hands out the next nonce and advances the counter, synchronously, so
    /// two requests can never share one.
    fn take_nonce(&mut self, chain_id: u64) -> u64 {
        let nonce = self.next_nonce(chain_id);
        let next = nonce
            .checked_add(1)
            .unwrap_or_else(|| env::panic_str("The nonce space is exhausted"));
        self.evm_nonces.insert(chain_id, next);
        nonce
    }
}

fn parse_address(text: &str) -> [u8; 20] {
    evm::parse_address(text)
        .unwrap_or_else(|| env::panic_str("Invalid EVM address: expected 0x and 40 hex digits"))
}

/// The signer's secp256k1 response, as observed:
/// `{"scheme":"Secp256k1","big_r":{"affine_point":"02.."},"s":{"scalar":".."},"recovery_id":1}`.
#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct SignerResponse {
    scheme: Option<String>,
    big_r: BigR,
    s: Scalar,
    recovery_id: u8,
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct BigR {
    affine_point: String,
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct Scalar {
    scalar: String,
}

/// Why a signer response was not turned into a transaction.
#[derive(Debug, PartialEq, Eq)]
enum Rejection {
    /// Not the JSON shape of a secp256k1 signature, or not hex.
    Malformed,
    /// Well-formed, but not a signature that can sign an Ethereum transaction.
    Invalid(EvmError),
}

/// Pure: from the signer's raw result bytes to a signed transaction. Never
/// panics on any input.
fn assemble_from_response(tx: &Eip1559Tx, response: &[u8]) -> Result<Vec<u8>, Rejection> {
    let response: SignerResponse =
        serde_json::from_slice(response).map_err(|_| Rejection::Malformed)?;
    if response
        .scheme
        .as_deref()
        .is_some_and(|scheme| scheme != "Secp256k1")
    {
        return Err(Rejection::Malformed);
    }
    let affine_point = evm::hex_decode(&response.big_r.affine_point).ok_or(Rejection::Malformed)?;
    let scalar = evm::hex_decode(&response.s.scalar).ok_or(Rejection::Malformed)?;
    evm::try_assemble_signed(tx, &affine_point, &scalar, response.recovery_id)
        .map_err(Rejection::Invalid)
}

/// NEP-297 event line carrying the raw signed transaction.
fn signed_payment_event(tx: &Eip1559Tx, raw_tx: &[u8]) -> String {
    let event = json!({
        "standard": "minder",
        "version": "1.0.0",
        "event": "evm_payment_signed",
        "data": [{
            "chain_id": tx.chain_id,
            "nonce": tx.nonce,
            "to": evm::format_address(&tx.to),
            "value": tx.value.to_string(),
            "raw_tx": format!("0x{}", evm::hex_encode(raw_tx)),
        }],
    });
    format!("EVENT_JSON:{event}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx() -> Eip1559Tx {
        Eip1559Tx {
            chain_id: 11_155_111,
            nonce: 0,
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 2_000_000_000,
            gas_limit: 21_000,
            to: [0x11; 20],
            value: 1000,
        }
    }

    const R: &str = "5b2fc0e1f6d4a7c98e3b1d2a4f5c6e7d8a9b0c1d2e3f405162738495a6b7c8d9";
    const S: &str = "2a4d6f8091b3d5f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f";

    fn response(
        affine_point: &str,
        scalar: &str,
        recovery_id: u8,
        scheme: Option<&str>,
    ) -> Vec<u8> {
        let mut value = json!({
            "big_r": { "affine_point": affine_point },
            "s": { "scalar": scalar },
            "recovery_id": recovery_id,
        });
        if let Some(scheme) = scheme {
            value["scheme"] = json!(scheme);
        }
        value.to_string().into_bytes()
    }

    #[test]
    fn a_well_formed_response_is_assembled() {
        let good = response(&format!("02{R}"), S, 1, Some("Secp256k1"));
        let raw = assemble_from_response(&tx(), &good).unwrap();
        assert_eq!(raw[0], 0x02, "typed transaction");
        // The scheme field is optional.
        assert_eq!(
            assemble_from_response(&tx(), &response(&format!("02{R}"), S, 1, None)).unwrap(),
            raw
        );
    }

    #[test]
    fn malformed_responses_are_rejected_without_panicking() {
        let t = tx();
        for bytes in [
            &b""[..],
            b"not json",
            b"{}",
            b"[]",
            b"null",
            br#"{"big_r":{"affine_point":"02"},"s":{"scalar":"00"}}"#,
        ] {
            assert_eq!(assemble_from_response(&t, bytes), Err(Rejection::Malformed));
        }
        // Wrong scheme, and non-hex or odd-length hex.
        assert_eq!(
            assemble_from_response(&t, &response(&format!("02{R}"), S, 1, Some("Ed25519"))),
            Err(Rejection::Malformed)
        );
        assert_eq!(
            assemble_from_response(&t, &response("zz", S, 1, None)),
            Err(Rejection::Malformed)
        );
        assert_eq!(
            assemble_from_response(&t, &response(&format!("02{R}"), "abc", 1, None)),
            Err(Rejection::Malformed)
        );
        // recovery_id outside a u8 does not even parse.
        let oversized = br#"{"big_r":{"affine_point":"02"},"s":{"scalar":"00"},"recovery_id":300}"#;
        assert_eq!(
            assemble_from_response(&t, oversized),
            Err(Rejection::Malformed)
        );
    }

    #[test]
    fn invalid_signatures_are_rejected_with_the_reason() {
        let t = tx();
        let bad_recovery = response(&format!("02{R}"), S, 2, None);
        assert_eq!(
            assemble_from_response(&t, &bad_recovery),
            Err(Rejection::Invalid(EvmError::RecoveryId))
        );
        let zero_s = response(&format!("02{R}"), &"00".repeat(32), 0, None);
        assert_eq!(
            assemble_from_response(&t, &zero_s),
            Err(Rejection::Invalid(EvmError::Scalar))
        );
        let bad_prefix = response(&format!("04{R}"), S, 0, None);
        assert_eq!(
            assemble_from_response(&t, &bad_prefix),
            Err(Rejection::Invalid(EvmError::AffinePoint))
        );
    }

    #[test]
    fn the_event_carries_the_transaction_fields_and_raw_bytes() {
        let line = signed_payment_event(&tx(), &[0x02, 0xab, 0xcd]);
        let json: serde_json::Value =
            serde_json::from_str(line.strip_prefix("EVENT_JSON:").unwrap()).unwrap();
        assert_eq!(json["standard"], "minder");
        assert_eq!(json["event"], "evm_payment_signed");
        let data = &json["data"][0];
        assert_eq!(data["chain_id"], 11_155_111);
        assert_eq!(data["nonce"], 0);
        assert_eq!(data["to"], format!("0x{}", "11".repeat(20)));
        assert_eq!(data["value"], "1000");
        assert_eq!(data["raw_tx"], "0x02abcd");
    }

    // ---- Fuzz: arbitrary bytes never panic and always yield a Result ----

    use proptest::prelude::*;

    use crate::evm::try_assemble_signed;

    const FUZZ_CASES: u32 = 10_000;
    /// The secp256k1 group order, big-endian.
    const ORDER: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36,
        0x41, 0x41,
    ];

    /// The documented acceptance rule for a raw signature, restated
    /// independently of `try_assemble_signed`.
    fn is_acceptable(point: &[u8], scalar: &[u8], recovery_id: u8) -> bool {
        let in_range =
            |value: &[u8]| value.len() == 32 && value.iter().any(|b| *b != 0) && value < &ORDER[..];
        recovery_id <= 1
            && in_range(scalar)
            && point.len() == 33
            && (point[0] == 0x02 || point[0] == 0x03)
            && in_range(&point[1..])
    }

    /// 32-byte values biased to the edges of a scalar: valid ones (random,
    /// the fixture, half the order, one, order minus one) and the invalid ones
    /// around them (zero, the order and its neighbour, all ones), plus values
    /// of the wrong length. Weighted so both outcomes occur in volume.
    fn edge_scalar() -> impl Strategy<Value = Vec<u8>> {
        let hex = |text: &str| evm::hex_decode(text).unwrap();
        prop_oneof![
            3 => proptest::collection::vec(any::<u8>(), 32),
            2 => Just(hex(S)),
            1 => Just(hex("7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0")),
            1 => Just(hex(&format!("{}1", "0".repeat(63)))),
            1 => Just(hex("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364140")),
            1 => Just(vec![0u8; 32]),
            1 => Just(ORDER.to_vec()),
            1 => Just(hex("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364142")),
            1 => Just(vec![0xff; 32]),
            1 => proptest::collection::vec(any::<u8>(), 0..40),
        ]
    }

    #[test]
    fn assemble_from_response_never_panics_on_arbitrary_bytes() {
        let tx = tx();
        let mut runner = crate::model_tests::seeded_runner(FUZZ_CASES);
        runner
            .run(&proptest::collection::vec(any::<u8>(), 0..6000), |bytes| {
                if let Ok(raw) = assemble_from_response(&tx, &bytes) {
                    prop_assert_eq!(raw[0], 0x02);
                }
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn assemble_from_response_never_panics_on_mutated_real_signatures() {
        // Random bytes almost never reach past the JSON parser. Mutating a
        // real response (flips, truncation, trailing bytes) does.
        let tx = tx();
        let real = response(&format!("02{R}"), S, 1, Some("Secp256k1"));
        let length = real.len();
        let mutations = (
            0..=length,
            proptest::collection::vec((0..length, any::<u8>()), 0..8),
            proptest::collection::vec(any::<u8>(), 0..20),
        );
        let mut runner = crate::model_tests::seeded_runner(FUZZ_CASES);
        runner
            .run(&mutations, |(cut, flips, tail)| {
                let mut bytes = real.clone();
                for (index, value) in flips {
                    bytes[index] = value;
                }
                bytes.truncate(cut);
                bytes.extend(tail);
                if let Ok(raw) = assemble_from_response(&tx, &bytes) {
                    prop_assert_eq!(raw[0], 0x02);
                }
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn assemble_from_response_accepts_exactly_the_acceptable_signatures() {
        // Well-formed JSON around random fields: the result must be Ok exactly
        // when the independent rule says the signature is acceptable.
        let tx = tx();
        let recovery_ids = prop_oneof![4 => 0u32..2, 1 => 2u32..400];
        let fields = (edge_scalar(), edge_scalar(), recovery_ids, any::<bool>());
        let (accepted, rejected) = (std::cell::Cell::new(0u32), std::cell::Cell::new(0u32));
        let mut runner = crate::model_tests::seeded_runner(FUZZ_CASES);
        runner
            .run(
                &fields,
                |(point_tail, scalar, recovery_id, prefix_three)| {
                    let mut point = vec![if prefix_three { 0x03 } else { 0x02 }];
                    point.extend(&point_tail);
                    let body = response(
                        &evm::hex_encode(&point),
                        &evm::hex_encode(&scalar),
                        0, // replaced below: recovery_id may exceed a u8
                        None,
                    );
                    let mut value: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    value["recovery_id"] = json!(recovery_id);
                    let bytes = value.to_string().into_bytes();

                    let expected =
                        recovery_id <= 255 && is_acceptable(&point, &scalar, recovery_id as u8);
                    let result = assemble_from_response(&tx, &bytes).is_ok();
                    prop_assert_eq!(result, expected);
                    let counter = if result { &accepted } else { &rejected };
                    counter.set(counter.get() + 1);
                    Ok(())
                },
            )
            .unwrap();
        // Non-vacuous: the generator reaches both outcomes in volume.
        assert!(
            accepted.get() > 500 && rejected.get() > 500,
            "accepted {}, rejected {}",
            accepted.get(),
            rejected.get()
        );
    }

    #[test]
    fn try_assemble_signed_never_panics_and_accepts_exactly_the_acceptable_signatures() {
        let tx = tx();
        let raw_inputs = (
            proptest::collection::vec(any::<u8>(), 0..70),
            edge_scalar(),
            any::<u8>(),
        );
        let structured = (
            edge_scalar(),
            edge_scalar(),
            prop_oneof![3 => Just(0u8), 3 => Just(1u8), 1 => Just(2u8), 1 => any::<u8>()],
            prop_oneof![4 => Just(0x02u8), 4 => Just(0x03u8), 1 => any::<u8>()],
        )
            .prop_map(|(point_tail, scalar, recovery_id, prefix)| {
                let mut point = vec![prefix];
                point.extend(point_tail);
                (point, scalar, recovery_id)
            });
        let (accepted, rejected) = (std::cell::Cell::new(0u32), std::cell::Cell::new(0u32));
        let mut runner = crate::model_tests::seeded_runner(FUZZ_CASES);
        runner
            .run(
                &prop_oneof![raw_inputs, structured],
                |(point, scalar, recovery_id)| {
                    let result = try_assemble_signed(&tx, &point, &scalar, recovery_id);
                    prop_assert_eq!(result.is_ok(), is_acceptable(&point, &scalar, recovery_id));
                    let counter = if result.is_ok() { &accepted } else { &rejected };
                    counter.set(counter.get() + 1);
                    if let Ok(raw) = result {
                        prop_assert_eq!(raw[0], 0x02);
                    }
                    Ok(())
                },
            )
            .unwrap();
        // Non-vacuous: the generator reaches both outcomes in volume.
        assert!(
            accepted.get() > 500 && rejected.get() > 500,
            "accepted {}, rejected {}",
            accepted.get(),
            rejected.get()
        );
    }
}
