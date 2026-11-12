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
use near_sdk::{env, near, require, AccountId, Gas, NearToken, Promise, PromiseError, PublicKey};

use crate::evm::{self, Eip1559Tx, EvmError, Signature};
use crate::policy::RecipientRef;
use crate::types::{
    AssetId, Decision, EvmConfig, EvmConfigView, TokenPaymentDecision, DEFAULT_TOKEN_GAS_LIMIT,
};
use crate::{page, Minder, MinderExt};

/// Event names. An owner withdrawal is told apart from an agent payment, since
/// it is outside policy.
const PAYMENT_EVENT: &str = "evm_payment_signed";
const OWNER_WITHDRAWAL_EVENT: &str = "evm_owner_withdrawal_signed";

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

    /// Sets how EVM payments are built. `signer_id`, `derivation_path` and
    /// `sender` identify the signing key, so they are fixed once set: the nonce
    /// counter is only meaningful for one sending address, and `sender` is what
    /// every signature is checked against. `sender` is this contract's own EVM
    /// address, the one the signer derives for this contract account and
    /// `derivation_path` (`0x` and 40 hex digits). Chain id and fees may be
    /// changed. `gas_limit` defaults to 21000, a plain ETH transfer;
    /// `token_gas_limit` (the gas of an ERC-20 `transfer`, also changeable)
    /// defaults to 100000.
    // The flat argument list is the call's JSON interface; grouping the fields
    // into one object would change it for every caller.
    #[allow(clippy::too_many_arguments)]
    pub fn set_evm_config(
        &mut self,
        signer_id: AccountId,
        derivation_path: String,
        sender: String,
        chain_id: u64,
        max_fee_per_gas: U128,
        max_priority_fee_per_gas: U128,
        gas_limit: Option<u64>,
        token_gas_limit: Option<u64>,
    ) {
        self.assert_owner();
        let config = EvmConfig {
            signer_id,
            derivation_path,
            sender: parse_address(&sender),
            chain_id,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            gas_limit: gas_limit.unwrap_or(DEFAULT_GAS_LIMIT),
            token_gas_limit: token_gas_limit.unwrap_or(DEFAULT_TOKEN_GAS_LIMIT),
        };
        require!(config.chain_id > 0, "chain_id must be positive");
        require!(config.gas_limit > 0, "gas_limit must be positive");
        require!(
            config.token_gas_limit > 0,
            "token_gas_limit must be positive"
        );
        require!(
            config.sender != [0u8; 20],
            "sender must not be the zero address"
        );
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
        require!(
            config
                .max_fee_per_gas
                .0
                .checked_mul(u128::from(config.token_gas_limit))
                .is_some(),
            "max_fee_per_gas * token_gas_limit overflows"
        );
        if let Some(existing) = &self.evm_config {
            require!(
                existing.signer_id == config.signer_id
                    && existing.derivation_path == config.derivation_path
                    && existing.sender == config.sender,
                "signer_id, derivation_path and sender cannot change once set"
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

    /// Owner recovery for the derived EVM address: signs a transfer of `value`
    /// wei to `recipient` at the explicit `nonce`, for any recipient and amount.
    ///
    /// Like `owner_withdraw` on the NEAR side it is owner-only, works in any
    /// status including `Frozen`, and is entirely outside policy: no cap,
    /// allowlist or window check, and it neither reserves nor books anything.
    /// Minder contains the agent, not the owner.
    ///
    /// The nonce is the owner's, because the second job is cancellation: a
    /// transaction at an already-issued nonce replaces a signed-but-unbroadcast
    /// or stuck agent transaction. For that, raise the fees with
    /// `set_evm_config` first; the transaction takes its fees from the
    /// configuration, and a replacement must pay more than what it replaces.
    /// This method is therefore deliberately outside the agent path's
    /// unique-nonce rule. To keep the *agent* from colliding with a nonce used
    /// here, the counter moves to `max(counter, nonce + 1)`: a fresh or future
    /// nonce advances it, a replacement of an issued nonce leaves it alone.
    ///
    /// The contract builds the transaction itself from the recipient, value,
    /// nonce and configured chain and fees, and computes the hash itself (no
    /// blind signing). `on_owner_sign_complete` applies the same authenticity
    /// check as the agent path before anything is emitted.
    pub fn owner_evm_withdraw(&mut self, recipient: String, value: U128, nonce: U64) -> Promise {
        self.assert_owner();
        let config = self.require_evm_config();
        let to = parse_address(&recipient);
        let after = nonce
            .0
            .checked_add(1)
            .unwrap_or_else(|| env::panic_str("The nonce space is exhausted"));
        if after > self.next_nonce(config.chain_id) {
            self.evm_nonces.insert(config.chain_id, after);
        }

        let tx = config.build_tx(nonce.0, to, value.0);
        let hash = evm::signing_hash(&tx);

        Promise::new(config.signer_id.clone())
            .function_call(
                "sign".to_string(),
                signing_request_args(&config, &hash),
                SIGN_DEPOSIT,
                SIGN_GAS,
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(SIGN_CALLBACK_GAS)
                    .on_owner_sign_complete(tx),
            )
    }

    /// Finishes `owner_evm_withdraw`. There is nothing to release or book, so
    /// it changes no state; it only decides whether to emit. The transaction is
    /// emitted for an off-chain relayer iff the signature assembles and
    /// recovers to the configured `sender`; otherwise the reason is logged and
    /// nothing is emitted. Must never panic.
    #[private]
    pub fn on_owner_sign_complete(&self, #[serializer(borsh)] tx: Eip1559Tx) {
        let response = match env::promise_result_checked(0, MAX_SIGNATURE_RESPONSE_BYTES) {
            Ok(bytes) => Some(bytes),
            Err(PromiseError::Failed) => {
                env::log_str("owner evm withdrawal not signed: the signer call failed");
                return;
            }
            Err(_) => None,
        };
        match self.authenticate(&tx, response.as_deref()) {
            Ok(raw_tx) => env::log_str(&signed_tx_event(OWNER_WITHDRAWAL_EVENT, &tx, &raw_tx)),
            Err(fault) => env::log_str(&format!(
                "owner evm withdrawal not emitted: {}",
                fault.describe()
            )),
        }
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
        let caller_key = Self::authenticated_key();
        let asset = AssetId::Evm(config.chain_id);
        let outlay = config.outlay(amount.0);

        let recipient_allowed = self.evm_recipients.contains(&to);
        if let Decision::Deny(reason) = self.evaluate_with(
            caller_key.as_ref(),
            &asset,
            outlay,
            RecipientRef::Evm(&to),
            recipient_allowed,
        ) {
            env::panic_str(&format!("Denied: {reason:?}"));
        }

        self.reserve(&asset, outlay);

        let nonce = self.take_nonce(config.chain_id);
        let tx = config.build_tx(nonce, to, amount.0);
        let hash = evm::signing_hash(&tx);

        Promise::new(config.signer_id.clone())
            .function_call(
                "sign".to_string(),
                signing_request_args(&config, &hash),
                SIGN_DEPOSIT,
                SIGN_GAS,
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(SIGN_CALLBACK_GAS)
                    .on_sign_complete(vec![(asset, outlay)], tx),
            )
    }

    /// Pays `amount` of the ERC-20 `token` to `recipient` on the configured
    /// chain, if policy allows, by getting the Chain Signatures contract to
    /// sign a call to `token.transfer(recipient, amount)` that this contract
    /// builds itself. The agent supplies only (token, recipient, amount): the
    /// calldata is the `transfer` selector, the padded recipient and the amount,
    /// and nothing else. No raw calldata is accepted and no other function is
    /// ever called (no `approve`, no general decoding).
    ///
    /// The firewall inspects what the transaction does, so a payment has two
    /// legs, each evaluated and reserved before any promise exists:
    /// - the **transfer**: `amount` in the token's units, against the token's
    ///   own policy (an `Evm20` asset), the EVM recipient allowlist for the
    ///   decoded `recipient`, and the mandate's scope;
    /// - the **gas**: `token_gas_limit * max_fee_per_gas` wei, against the
    ///   chain's native ETH policy, which must exist.
    ///
    /// A denial of either reverts with its reason and the leg. On allow both
    /// are reserved; `on_sign_complete` releases both and, under the same G1
    /// rule as a native payment, books both whenever the signer's receipt did
    /// not fail.
    pub fn request_evm_token_payment(
        &mut self,
        token: String,
        recipient: String,
        amount: U128,
    ) -> Promise {
        let config = self.require_evm_config();
        let token_address = parse_address(&token);
        let to = parse_address(&recipient);
        let caller_key = Self::authenticated_key();
        let token_asset = AssetId::Evm20 {
            chain_id: config.chain_id,
            token: token_address,
        };
        let gas_asset = AssetId::Evm(config.chain_id);
        let gas = config.token_fee_cost();
        require!(
            self.policies.contains_key(&gas_asset),
            "A token transfer needs a native ETH policy for its gas: none is set"
        );

        let recipient_allowed = self.evm_recipients.contains(&to);
        if let Decision::Deny(reason) = self.evaluate_with(
            caller_key.as_ref(),
            &token_asset,
            amount.0,
            RecipientRef::Evm(&to),
            recipient_allowed,
        ) {
            env::panic_str(&format!("Denied: {reason:?} (token transfer)"));
        }
        if let Decision::Deny(reason) = self.evaluate_gas(caller_key.as_ref(), &gas_asset, gas) {
            env::panic_str(&format!("Denied: {reason:?} (gas, native ETH)"));
        }

        self.reserve(&token_asset, amount.0);
        self.reserve(&gas_asset, gas);

        let nonce = self.take_nonce(config.chain_id);
        let tx = config.build_token_tx(nonce, token_address, &to, amount.0);
        let hash = evm::signing_hash(&tx);

        Promise::new(config.signer_id.clone())
            .function_call(
                "sign".to_string(),
                signing_request_args(&config, &hash),
                SIGN_DEPOSIT,
                SIGN_GAS,
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(SIGN_CALLBACK_GAS)
                    .on_sign_complete(vec![(token_asset, amount.0), (gas_asset, gas)], tx),
            )
    }

    /// Settles a payment started by `request_evm_payment` or
    /// `request_evm_token_payment`. Must never panic, or a reservation would be
    /// stranded.
    ///
    /// `reservations` are the `(asset, amount)` pairs the request reserved: one
    /// for a native payment, two for a token payment (the token amount and the
    /// native gas). Each is released exactly once, first and unconditionally,
    /// and each is booked or not by the same single rule. Whether to book the
    /// spend depends only on whether the signer's receipt *succeeded*,
    /// never on whether this contract could read, size-fit, parse, assemble or
    /// authenticate what it returned:
    /// - **Receipt failed** (`PromiseError::Failed`, which includes the signer's
    ///   own timeout): nothing was signed. Nothing is booked.
    /// - **Receipt did not fail**: the signer produced output that sits in a
    ///   public receipt and may be broadcastable by anyone, whether or not we can
    ///   use it. Every reserved amount is booked unconditionally.
    ///
    /// Emission is a separate, stricter matter. The raw signed transaction is
    /// emitted for an off-chain relayer only if the output assembles into a
    /// valid transaction **and** `ecrecover` over the hash this contract
    /// computed, with the signature exactly as it goes into the transaction,
    /// yields the configured `sender`. Format validation shows a signature is
    /// well formed; this shows it is authentically ours. Otherwise the spend
    /// stays booked, the reason is logged (an authenticity failure is named as a
    /// signer or configuration fault) and nothing is emitted.
    ///
    /// The arguments are borsh, not JSON: they are produced only by this
    /// contract, and the transaction carries `u128` fields.
    #[private]
    pub fn on_sign_complete(
        &mut self,
        #[serializer(borsh)] reservations: Vec<(AssetId, u128)>,
        #[serializer(borsh)] tx: Eip1559Tx,
    ) {
        for (asset, amount) in &reservations {
            self.release(asset, *amount);
        }

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
        // Nothing below changes this: ecrecover decides emission, not booking.
        for (asset, amount) in &reservations {
            self.record_spend(asset, *amount);
        }

        match self.authenticate(&tx, response.as_deref()) {
            Ok(raw_tx) => env::log_str(&signed_tx_event(PAYMENT_EVENT, &tx, &raw_tx)),
            Err(fault) => env::log_str(&format!(
                "evm payment booked, no transaction emitted: {}",
                fault.describe()
            )),
        }
    }

    // ---- Views ----

    /// `dry_run` for EVM payments: what the holder of `agent_key` would get from
    /// `request_evm_payment` right now, counting in-flight reservations.
    pub fn dry_run_evm(&self, agent_key: PublicKey, recipient: String, amount: U128) -> Decision {
        let config = self.require_evm_config();
        let to = parse_address(&recipient);
        self.evaluate_with(
            Some(&agent_key),
            &AssetId::Evm(config.chain_id),
            config.outlay(amount.0),
            RecipientRef::Evm(&to),
            self.evm_recipients.contains(&to),
        )
    }

    /// `dry_run` for token payments: what the holder of `agent_key` would get
    /// from `request_evm_token_payment` right now, counting in-flight
    /// reservations, for each of its two legs.
    pub fn dry_run_evm_token(
        &self,
        agent_key: PublicKey,
        token: String,
        recipient: String,
        amount: U128,
    ) -> TokenPaymentDecision {
        let config = self.require_evm_config();
        let token_address = parse_address(&token);
        let to = parse_address(&recipient);
        TokenPaymentDecision {
            token: self.evaluate_with(
                Some(&agent_key),
                &AssetId::Evm20 {
                    chain_id: config.chain_id,
                    token: token_address,
                },
                amount.0,
                RecipientRef::Evm(&to),
                self.evm_recipients.contains(&to),
            ),
            gas: self.evaluate_gas(
                Some(&agent_key),
                &AssetId::Evm(config.chain_id),
                config.token_fee_cost(),
            ),
        }
    }

    pub fn get_evm_config(&self) -> Option<EvmConfigView> {
        self.evm_config.as_ref().map(EvmConfigView::from)
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

    /// From the signer's result to the raw signed transaction, or why not.
    /// Shared by both callbacks, so the agent path and the owner path apply the
    /// same authenticity gate: the signature must assemble and `ecrecover` over
    /// the hash this contract computed (never a hash from the response), with the
    /// signature exactly as it goes into the transaction, must yield the
    /// configured `sender`. A missing sender cannot happen once a request was
    /// accepted, but it fails closed all the same.
    fn authenticate(&self, tx: &Eip1559Tx, response: Option<&[u8]>) -> Result<Vec<u8>, Fault> {
        let response = response.ok_or(Fault::Unreadable)?;
        let signature = parse_signature(response).map_err(Fault::Rejected)?;
        let recovered = evm::recover_address(&evm::signing_hash(tx), &signature);
        let sender = self.evm_config.as_ref().map(|config| config.sender);
        if recovered.is_some() && recovered == sender {
            Ok(evm::encode_signed(
                tx,
                signature.r,
                signature.s,
                signature.y_parity,
            ))
        } else {
            Err(Fault::Inauthentic { recovered, sender })
        }
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

/// Pure: from the signer's raw result bytes to the signature that goes into a
/// transaction (validated and normalized to low-s). Never panics on any input.
fn parse_signature(response: &[u8]) -> Result<Signature, Rejection> {
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
    evm::normalized_signature(&affine_point, &scalar, response.recovery_id)
        .map_err(Rejection::Invalid)
}

/// Why a signer result did not become an emitted transaction.
#[derive(Debug, PartialEq, Eq)]
enum Fault {
    /// The receipt succeeded but its output exceeded the read buffer.
    Unreadable,
    Rejected(Rejection),
    /// A well-formed signature that does not recover to the configured sender.
    Inauthentic {
        recovered: Option<[u8; 20]>,
        sender: Option<[u8; 20]>,
    },
}

impl Fault {
    /// The reason as logged. An authenticity failure shows the recovered
    /// address because it is what the owner needs to find the fault: a wrong
    /// `sender` in the configuration, a wrong derivation path, or a signer
    /// signing with another key.
    fn describe(&self) -> String {
        match self {
            Fault::Unreadable => "unreadable signer result".to_string(),
            Fault::Rejected(rejection) => format!("signature rejected ({rejection:?})"),
            Fault::Inauthentic { recovered, sender } => {
                let show = |address: &Option<[u8; 20]>, absent: &str| {
                    address
                        .as_ref()
                        .map_or(absent.to_string(), evm::format_address)
                };
                format!(
                    "authenticity failure, the signature recovers to {} but the configured \
                     sender is {}; this is a signer or configuration fault",
                    show(recovered, "no address"),
                    show(sender, "unset"),
                )
            }
        }
    }
}

/// The body of the call to the signer's `sign`: the key path and the hash the
/// contract computed. Kept apart from the dispatch itself, which the
/// signing-confinement test pins to the functions allowed to make it.
fn signing_request_args(config: &EvmConfig, hash: &[u8; 32]) -> Vec<u8> {
    json!({
        "request": {
            "path": config.derivation_path,
            "payload_v2": { "Ecdsa": evm::hex_encode(hash) },
            "domain_id": SECP256K1_DOMAIN_ID,
        }
    })
    .to_string()
    .into_bytes()
}

/// NEP-297 event line carrying the raw signed transaction.
fn signed_tx_event(event: &str, tx: &Eip1559Tx, raw_tx: &[u8]) -> String {
    let event = json!({
        "standard": "minder",
        "version": "1.0.0",
        "event": event,
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
            data: Vec::new(),
        }
    }

    const R: &str = "5b2fc0e1f6d4a7c98e3b1d2a4f5c6e7d8a9b0c1d2e3f405162738495a6b7c8d9";
    const S: &str = "2a4d6f8091b3d5f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f";

    /// The assembly half of the callback, without the authenticity check:
    /// parse and normalize the signer's response, then encode the transaction.
    fn assemble_from_response(tx: &Eip1559Tx, response: &[u8]) -> Result<Vec<u8>, Rejection> {
        parse_signature(response)
            .map(|signature| evm::encode_signed(tx, signature.r, signature.s, signature.y_parity))
    }

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
        let line = signed_tx_event(PAYMENT_EVENT, &tx(), &[0x02, 0xab, 0xcd]);
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
