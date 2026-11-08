//! Test double for a NEP-141 token. Only what the Minder tests need: a plain
//! `ft_transfer` that checks the 1 yoctoNEAR deposit and records the transfer.
//! It deliberately has no `ft_transfer_call`, so any attempt to use it fails.

use near_sdk::json_types::U128;
use near_sdk::{assert_one_yocto, env, near, require, AccountId, PanicOnDefault, PromiseOrValue};

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
