//! Fixtures shared by the unit tests that run under the mocked environment.

use near_sdk::json_types::{U128, U64};
use near_sdk::mock::{MockAction, Receipt};
use near_sdk::serde_json::Value;
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{testing_env, AccountId, Gas, NearToken};

use crate::{AssetId, Minder};

pub const PER_TX_CAP: u128 = 50;
pub const WINDOW_LIMIT: u128 = 100;

pub fn account(name: &str) -> AccountId {
    name.parse().unwrap()
}

pub fn token() -> AssetId {
    AssetId::Nep141(account("usdc.near"))
}

pub fn context(predecessor: &str) -> VMContextBuilder {
    let mut builder = VMContextBuilder::new();
    builder
        .current_account_id(account("minder.near"))
        .predecessor_account_id(account(predecessor))
        .block_timestamp(1_000_000)
        .prepaid_gas(Gas::from_tgas(300));
    builder
}

/// Initialized, with policies for NEAR and a token, an allowed recipient and a
/// mandate for `agent.near`. The environment is left as the agent.
pub fn armed_as_agent() -> Minder {
    testing_env!(context("owner.near").build());
    let mut minder = Minder::init(account("owner.near"), U64(400), 4);
    for asset in [AssetId::Near, token()] {
        minder.set_asset_policy(asset, U128(PER_TX_CAP), U128(WINDOW_LIMIT));
    }
    minder.set_recipient(account("bob.near"), true);
    minder.grant_mandate(account("agent.near"), U64(u64::MAX));
    testing_env!(context("agent.near").build());
    minder
}

pub fn reserved(minder: &Minder, asset: AssetId) -> u128 {
    minder.get_reserved(asset).0
}

pub fn committed(minder: &Minder, asset: AssetId) -> u128 {
    minder.get_live_spend(asset).0
}

pub struct FunctionCall {
    /// The arguments as JSON, or `null` if they are not JSON (borsh callbacks).
    pub args: Value,
    pub raw_args: Vec<u8>,
    pub deposit: NearToken,
    pub gas: Gas,
}

pub fn function_call(receipts: &[Receipt], receiver: &str, method: &str) -> FunctionCall {
    receipts
        .iter()
        .filter(|receipt| receipt.receiver_id == account(receiver))
        .flat_map(|receipt| &receipt.actions)
        .find_map(|action| match action {
            MockAction::FunctionCallWeight {
                method_name,
                args,
                attached_deposit,
                prepaid_gas,
                ..
            } if method_name == method.as_bytes() => Some(FunctionCall {
                args: near_sdk::serde_json::from_slice(args).unwrap_or(Value::Null),
                raw_args: args.clone(),
                deposit: *attached_deposit,
                gas: *prepaid_gas,
            }),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {method} call to {receiver}"))
}

/// Which fixture key signs: `Sender` is the configured sender, `Other` is a
/// different key whose signatures are well formed but must not be emitted.
#[derive(Clone, Copy, Debug)]
pub enum Signer {
    Sender,
    Other,
}

/// One real signature from the committed fixture (`vectors/evm-signatures.json`),
/// as produced by an independent signer, with its reference serialization.
pub struct FixtureSignature {
    pub r: [u8; 32],
    pub s: [u8; 32],
    pub y_parity: u8,
    /// The signed transaction as serialized by viem.
    pub raw_tx: Vec<u8>,
}

fn fixture() -> &'static Value {
    static FIXTURE: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    FIXTURE.get_or_init(|| {
        near_sdk::serde_json::from_str(include_str!("../vectors/evm-signatures.json"))
            .expect("the signature fixture is valid JSON")
    })
}

fn decode_hex(text: &str) -> Vec<u8> {
    let digits = text.strip_prefix("0x").unwrap_or(text);
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).expect("fixture hex"))
        .collect()
}

/// The configured sender: the address of fixture key A.
pub fn test_sender() -> [u8; 20] {
    decode_hex(fixture()["header"]["sender_a"].as_str().unwrap())
        .try_into()
        .expect("20-byte address")
}

pub fn test_sender_hex() -> String {
    fixture()["header"]["sender_a"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The fixture signature by `signer` over the signing hash `hash`.
pub fn fixture_signature(hash: &[u8; 32], signer: Signer) -> FixtureSignature {
    let key = format!(
        "0x{}",
        hash.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    let name = match signer {
        Signer::Sender => "A",
        Signer::Other => "B",
    };
    let entry = &fixture()["signatures"][&key][name];
    assert!(
        !entry.is_null(),
        "no fixture signature for hash {key}; regenerate with vectors/gen-evm-signatures.mjs"
    );
    FixtureSignature {
        r: decode_hex(entry["r"].as_str().unwrap()).try_into().unwrap(),
        s: decode_hex(entry["s"].as_str().unwrap()).try_into().unwrap(),
        y_parity: entry["y_parity"].as_u64().unwrap() as u8,
        raw_tx: decode_hex(entry["raw_tx"].as_str().unwrap()),
    }
}

/// The signer's JSON response carrying `signature`, as the MPC contract
/// returns it. The contract reads only the x coordinate of `big_r` (the
/// prefix is not consulted beyond being well formed); parity is `recovery_id`.
pub fn signer_response(signature: &FixtureSignature) -> Vec<u8> {
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    near_sdk::serde_json::json!({
        "scheme": "Secp256k1",
        "big_r": { "affine_point": format!("02{}", hex(&signature.r)) },
        "s": { "scalar": hex(&signature.s) },
        "recovery_id": signature.y_parity,
    })
    .to_string()
    .into_bytes()
}
