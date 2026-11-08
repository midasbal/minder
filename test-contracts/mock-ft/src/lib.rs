//! Test double for a NEP-141 token. Only what the Minder tests need: a plain
//! `ft_transfer` that checks the 1 yoctoNEAR deposit and records the transfer,
//! and an `ft_transfer_call` that drives a receiver's `ft_on_transfer` and
//! records only the amount the receiver kept. It tracks no balances.

use near_sdk::json_types::U128;
use near_sdk::serde_json::json;
use near_sdk::{
    assert_one_yocto, env, near, require, AccountId, Gas, NearToken, PanicOnDefault, Promise,
    PromiseOrValue,
};

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct MockFt {
    fail_transfers: bool,
    /// Extra cross-contract hops before a transfer completes. Each hop is a
    /// block, so this stretches how long the transfer stays in flight.
    slow_hops: u32,
    transfers: Vec<(AccountId, U128)>,
}

#[near]
impl MockFt {
    #[init]
    pub fn new() -> Self {
        Self {
            fail_transfers: false,
            slow_hops: 0,
            transfers: Vec::new(),
        }
    }

    pub fn set_fail_transfers(&mut self, fail: bool) {
        self.fail_transfers = fail;
    }

    pub fn set_slow_hops(&mut self, hops: u32) {
        self.slow_hops = hops;
    }

    #[payable]
    pub fn ft_transfer(
        &mut self,
        receiver_id: AccountId,
        amount: U128,
        #[allow(unused_variables)] memo: Option<String>,
    ) -> PromiseOrValue<()> {
        assert_one_yocto();
        require!(!self.fail_transfers, "mock: ft_transfer is set to fail");
        self.advance(self.slow_hops, receiver_id, amount)
    }

    #[payable]
    pub fn ft_transfer_call(
        &mut self,
        receiver_id: AccountId,
        amount: U128,
        #[allow(unused_variables)] memo: Option<String>,
        msg: String,
    ) -> Promise {
        assert_one_yocto();
        require!(
            !self.fail_transfers,
            "mock: ft_transfer_call is set to fail"
        );
        Promise::new(receiver_id.clone())
            .function_call(
                "ft_on_transfer".to_string(),
                json!({ "sender_id": env::predecessor_account_id(), "amount": amount, "msg": msg })
                    .to_string()
                    .into_bytes(),
                NearToken::from_yoctonear(0),
                Gas::from_tgas(30),
            )
            .then(Self::ext(env::current_account_id()).on_call_resolved(receiver_id, amount))
    }

    /// Records what the receiver kept (the amount minus what it reported as
    /// unused), or nothing if its hook failed or answered with garbage.
    #[private]
    pub fn on_call_resolved(&mut self, receiver_id: AccountId, amount: U128) -> U128 {
        let unused = match env::promise_result_checked(0, 64) {
            Ok(bytes) => near_sdk::serde_json::from_slice::<U128>(&bytes).map_or(amount.0, |u| u.0),
            Err(_) => amount.0,
        }
        .min(amount.0);
        let kept = amount.0 - unused;
        if kept > 0 {
            self.transfers.push((receiver_id, U128(kept)));
        }
        U128(kept)
    }

    #[private]
    pub fn hop(
        &mut self,
        remaining: u32,
        receiver_id: AccountId,
        amount: U128,
    ) -> PromiseOrValue<()> {
        self.advance(remaining, receiver_id, amount)
    }

    pub fn get_transfers(&self) -> Vec<(AccountId, U128)> {
        self.transfers.clone()
    }
}

impl MockFt {
    fn advance(
        &mut self,
        remaining: u32,
        receiver_id: AccountId,
        amount: U128,
    ) -> PromiseOrValue<()> {
        if remaining == 0 {
            self.transfers.push((receiver_id, amount));
            return PromiseOrValue::Value(());
        }
        PromiseOrValue::Promise(Self::ext(env::current_account_id()).hop(
            remaining - 1,
            receiver_id,
            amount,
        ))
    }
}
