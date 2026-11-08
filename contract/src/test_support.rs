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
    pub args: Value,
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
                args: near_sdk::serde_json::from_slice(args).unwrap(),
                deposit: *attached_deposit,
                gas: *prepaid_gas,
            }),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {method} call to {receiver}"))
}
