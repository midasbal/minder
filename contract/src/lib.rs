//! Minder: owner-managed limits, allowlists and a time-boxed agent mandate,
//! enforced by a single pure evaluation function. The agent moves value on NEAR
//! only through `request_local_transfer`, which reserves budget before the
//! transfer and settles it in a callback. Nothing here signs for other chains.

pub mod evm;
pub mod policy;
pub mod types;
pub mod window;

use near_sdk::json_types::{U128, U64};
use near_sdk::serde_json::json;
use near_sdk::store::{IterableMap, IterableSet, LookupMap};
use near_sdk::{
    env, near, require, AccountId, BorshStorageKey, Gas, NearToken, PanicOnDefault, Promise,
    PromiseError,
};

pub use types::{AssetId, AssetPolicy, Config, Decision, DenyReason, Mandate, Status};

use policy::EvalInput;
use window::SpendRing;

/// Bounds the ring stored per asset and the work done per evaluation.
const MAX_NUM_BUCKETS: u32 = 64;
const MAX_PAGE_SIZE: u32 = 100;

/// Gas for the token's `ft_transfer`. Gas for the settlement callback is
/// reserved separately and up front: if the callback could run out of gas the
/// reservation would never be released.
const FT_TRANSFER_GAS: Gas = Gas::from_tgas(30);
const SETTLE_CALLBACK_GAS: Gas = Gas::from_tgas(15);

#[near(serializers = [borsh])]
#[derive(BorshStorageKey)]
enum StorageKey {
    Policies,
    Recipients,
    Spend,
    Reserved,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Minder {
    config: Config,
    mandate: Option<Mandate>,
    /// Epoch of the most recently granted mandate; never decreases, and is
    /// kept across revocation so epochs are never reused.
    last_mandate_epoch: u64,
    policies: IterableMap<AssetId, AssetPolicy>,
    recipients: IterableSet<AccountId>,
    /// Spend history is kept when a policy is removed, so removing and
    /// re-adding a policy cannot reset the window.
    spend: LookupMap<AssetId, SpendRing>,
    /// Amount per asset reserved for transfers still in flight. Kept apart
    /// from `spend`: it is added when a transfer is requested and removed when
    /// its callback runs, whatever the outcome.
    reserved: LookupMap<AssetId, u128>,
}

#[near]
impl Minder {
    /// Single-use: `#[init]` panics if state already exists. The caller must
    /// be the designated owner or the contract account itself, so that nobody
    /// else can claim ownership by initializing first. Deploy and initialize
    /// in one batched transaction.
    #[init]
    pub fn init(owner: AccountId, window_len: U64, num_buckets: u32) -> Self {
        let caller = env::predecessor_account_id();
        require!(
            caller == owner || caller == env::current_account_id(),
            "init must be called by the owner or the contract account"
        );
        require!(
            (1..=MAX_NUM_BUCKETS).contains(&num_buckets),
            "num_buckets must be between 1 and 64"
        );
        require!(
            window_len.0 > 0 && window_len.0.is_multiple_of(u64::from(num_buckets)),
            "window_len must be positive and divisible by num_buckets"
        );
        Self {
            config: Config {
                owner,
                status: Status::Active,
                window_len,
                num_buckets,
            },
            mandate: None,
            last_mandate_epoch: 0,
            policies: IterableMap::new(StorageKey::Policies),
            recipients: IterableSet::new(StorageKey::Recipients),
            spend: LookupMap::new(StorageKey::Spend),
            reserved: LookupMap::new(StorageKey::Reserved),
        }
    }

    // ---- Owner-only ----

    /// Creates or replaces the policy for `asset`, which also allowlists it.
    pub fn set_asset_policy(&mut self, asset: AssetId, per_tx_cap: U128, window_limit: U128) {
        self.assert_owner();
        self.policies.insert(
            asset,
            AssetPolicy {
                per_tx_cap,
                window_limit,
            },
        );
    }

    /// Removes the policy, which un-allowlists the asset. Reverts if absent.
    pub fn remove_asset_policy(&mut self, asset: AssetId) {
        self.assert_owner();
        require!(
            self.policies.remove(&asset).is_some(),
            "No policy for this asset"
        );
    }

    pub fn set_recipient(&mut self, recipient: AccountId, allowed: bool) {
        self.assert_owner();
        if allowed {
            self.recipients.insert(recipient);
        } else {
            self.recipients.remove(&recipient);
        }
    }

    /// Replaces any existing mandate with a new one under a fresh epoch. This
    /// is the only way to restore authority after revocation or expiry.
    pub fn grant_mandate(&mut self, agent: AccountId, expiry: U64) {
        self.assert_owner();
        require!(
            expiry.0 > env::block_timestamp(),
            "Mandate expiry must be in the future"
        );
        self.last_mandate_epoch += 1;
        self.mandate = Some(Mandate {
            agent,
            expiry,
            epoch: self.last_mandate_epoch,
            revoked: false,
        });
    }

    /// Reverts if there is no mandate. Revoking an already revoked mandate is
    /// a no-op.
    pub fn revoke_mandate(&mut self) {
        self.assert_owner();
        let mandate = self.mandate.as_mut().unwrap_or_else(|| {
            env::panic_str("No mandate to revoke");
        });
        mandate.revoked = true;
    }

    pub fn freeze(&mut self) {
        self.assert_owner();
        self.config.status = Status::Frozen;
    }

    pub fn unfreeze(&mut self) {
        self.assert_owner();
        self.config.status = Status::Active;
    }

    /// Sends `amount` of `asset` from this contract to `to`. Owner recovery:
    /// it works in any status, including `Frozen`, and is deliberately outside
    /// policy. It checks no cap or allowlist, and neither reads nor changes
    /// `reserved` or the spend window. No callback: a failed transfer fails
    /// the returned promise and there is no accounting to undo. Minder contains
    /// the agent, not the owner.
    pub fn owner_withdraw(&mut self, asset: AssetId, amount: U128, to: AccountId) -> Promise {
        self.assert_owner();
        Self::transfer_promise(&asset, amount, to)
    }

    // ---- Funding ----

    /// Receives an attached NEAR deposit, which is credited to this contract's
    /// balance by the runtime. Exists to make funding explicit; open to anyone,
    /// since it only adds funds.
    #[payable]
    pub fn deposit(&mut self) {
        let attached = env::attached_deposit();
        require!(!attached.is_zero(), "Attach a deposit");
        env::log_str(&format!(
            "deposit {} yoctoNEAR from {}",
            attached.as_yoctonear(),
            env::predecessor_account_id()
        ));
    }

    /// NEP-141 receiver hook, called by a token on `ft_transfer_call`. Returns
    /// the amount not accepted. Tokens are accepted only from a token that has
    /// a policy, so this contract does not take on arbitrary tokens; any other
    /// caller gets everything back. The caller here is the token contract, not
    /// the sender, so `sender_id` and `msg` carry no authority and are ignored.
    pub fn ft_on_transfer(&self, sender_id: AccountId, amount: U128, msg: String) -> U128 {
        let _ = (sender_id, msg);
        let token = AssetId::Nep141(env::predecessor_account_id());
        if self.policies.contains_key(&token) {
            U128(0)
        } else {
            amount
        }
    }

    // ---- Agent ----

    /// Moves `amount` of `asset` from this contract to `recipient`, if policy
    /// allows. A denial reverts, so nothing changes. On allow, the amount is
    /// reserved before the transfer promise exists, so any later call already
    /// counts it. The reservation persists only because this method returns
    /// the promise; `on_transfer_complete` releases it.
    pub fn request_local_transfer(
        &mut self,
        asset: AssetId,
        amount: U128,
        recipient: AccountId,
    ) -> Promise {
        let caller = env::predecessor_account_id();
        if let Decision::Deny(reason) = self.evaluate_request(&caller, &asset, amount.0, &recipient)
        {
            env::panic_str(&format!("Denied: {reason:?}"));
        }

        self.reserve(&asset, amount.0);

        let transfer = Self::transfer_promise(&asset, amount, recipient);
        transfer.then(
            Self::ext(env::current_account_id())
                .with_static_gas(SETTLE_CALLBACK_GAS)
                .on_transfer_complete(asset, amount),
        )
    }

    /// Settles a transfer started by `request_local_transfer`. Must never
    /// panic, or the reservation would be stranded: it releases the
    /// reservation first and unconditionally, then commits the spend unless
    /// the transfer definitely failed. A result that is merely unexpected (for
    /// instance a token returning data) still counts as spent, because
    /// over-counting is safe and under-counting is not.
    #[private]
    pub fn on_transfer_complete(&mut self, asset: AssetId, amount: U128) {
        self.release(&asset, amount.0);
        if !matches!(env::promise_result_checked(0, 0), Err(PromiseError::Failed)) {
            self.record_spend(&asset, amount.0);
        }
    }

    // ---- Views ----

    /// Evaluates a hypothetical action as `caller` would experience it,
    /// counting in-flight reservations. A view has no authenticated caller, so
    /// the agent identity is an argument; `request_local_transfer` uses the
    /// predecessor account instead.
    pub fn dry_run(
        &self,
        caller: AccountId,
        asset: AssetId,
        amount: U128,
        recipient: AccountId,
    ) -> Decision {
        self.evaluate_request(&caller, &asset, amount.0, &recipient)
    }

    pub fn get_config(&self) -> Config {
        self.config.clone()
    }

    pub fn get_status(&self) -> Status {
        self.config.status
    }

    pub fn get_mandate(&self) -> Option<Mandate> {
        self.mandate.clone()
    }

    pub fn get_asset_policy(&self, asset: AssetId) -> Option<AssetPolicy> {
        self.policies.get(&asset).copied()
    }

    pub fn get_asset_policies(
        &self,
        from_index: Option<u32>,
        limit: Option<u32>,
    ) -> Vec<(AssetId, AssetPolicy)> {
        let (skip, take) = page(from_index, limit);
        self.policies
            .iter()
            .skip(skip)
            .take(take)
            .map(|(asset, policy)| (asset.clone(), *policy))
            .collect()
    }

    pub fn is_recipient_allowed(&self, recipient: AccountId) -> bool {
        self.recipients.contains(&recipient)
    }

    pub fn get_recipients(&self, from_index: Option<u32>, limit: Option<u32>) -> Vec<AccountId> {
        let (skip, take) = page(from_index, limit);
        self.recipients
            .iter()
            .skip(skip)
            .take(take)
            .cloned()
            .collect()
    }

    /// Committed spend for `asset` inside the live window right now.
    pub fn get_live_spend(&self, asset: AssetId) -> U128 {
        U128(self.live_spend(&asset))
    }

    /// Amount of `asset` reserved for transfers still in flight.
    pub fn get_reserved(&self, asset: AssetId) -> U128 {
        U128(self.reserved_of(&asset))
    }

    // ---- Test support ----

    /// Seeds committed spend without moving value. Compiled only with the
    /// `test-helpers` feature.
    #[cfg(feature = "test-helpers")]
    pub fn test_record_spend(&mut self, asset: AssetId, amount: U128) {
        self.assert_owner();
        self.record_spend(&asset, amount.0);
    }
}

impl Minder {
    /// The transfer itself, shared by the agent path and owner withdrawal:
    /// a native transfer, or a plain `ft_transfer` with the 1 yoctoNEAR the
    /// standard requires. Never `ft_transfer_call`, which would hand control
    /// to the recipient.
    fn transfer_promise(asset: &AssetId, amount: U128, recipient: AccountId) -> Promise {
        match asset {
            AssetId::Near => Promise::new(recipient).transfer(NearToken::from_yoctonear(amount.0)),
            AssetId::Nep141(token) => Promise::new(token.clone()).function_call(
                "ft_transfer".to_string(),
                json!({ "receiver_id": recipient, "amount": amount, "memo": null })
                    .to_string()
                    .into_bytes(),
                NearToken::from_yoctonear(1),
                FT_TRANSFER_GAS,
            ),
        }
    }

    fn assert_owner(&self) {
        require!(
            env::predecessor_account_id() == self.config.owner,
            "Only the owner can call this method"
        );
    }

    fn slot_len(&self) -> u64 {
        self.config.window_len.0 / u64::from(self.config.num_buckets)
    }

    /// Committed spend inside the live window.
    fn live_spend(&self, asset: &AssetId) -> u128 {
        self.spend.get(asset).map_or(0, |ring| {
            ring.live_sum(env::block_timestamp(), self.slot_len())
        })
    }

    fn reserved_of(&self, asset: &AssetId) -> u128 {
        self.reserved.get(asset).copied().unwrap_or(0)
    }

    fn reserve(&mut self, asset: &AssetId, amount: u128) {
        let total = self.reserved_of(asset).saturating_add(amount);
        self.reserved.insert(asset.clone(), total);
    }

    /// Saturating, so a release can never panic.
    fn release(&mut self, asset: &AssetId, amount: u128) {
        let remaining = self.reserved_of(asset).saturating_sub(amount);
        if remaining == 0 {
            self.reserved.remove(asset);
        } else {
            self.reserved.insert(asset.clone(), remaining);
        }
    }

    /// Commits spend into the window slot containing the current block.
    fn record_spend(&mut self, asset: &AssetId, amount: u128) {
        let (now, slot_len) = (env::block_timestamp(), self.slot_len());
        let num_buckets = self.config.num_buckets;
        self.spend
            .entry(asset.clone())
            .or_insert_with(|| SpendRing::new(num_buckets))
            .record(now, slot_len, amount);
    }

    /// Gathers state and defers every decision to [`policy::evaluate`]. Used
    /// by both `dry_run` and `request_local_transfer`.
    fn evaluate_request(
        &self,
        caller: &AccountId,
        asset: &AssetId,
        amount: u128,
        recipient: &AccountId,
    ) -> Decision {
        policy::evaluate(&EvalInput {
            status: self.config.status,
            mandate: self.mandate.as_ref(),
            now: env::block_timestamp(),
            caller,
            policy: self.policies.get(asset),
            recipient_allowed: self.recipients.contains(recipient),
            amount,
            effective_spend: policy::effective_spend(
                self.live_spend(asset),
                self.reserved_of(asset),
            ),
        })
    }
}

fn page(from_index: Option<u32>, limit: Option<u32>) -> (usize, usize) {
    let skip = from_index.unwrap_or(0) as usize;
    let take = limit.unwrap_or(MAX_PAGE_SIZE).min(MAX_PAGE_SIZE) as usize;
    (skip, take)
}

#[cfg(test)]
mod custody_tests;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod test_support;
