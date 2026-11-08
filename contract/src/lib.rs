//! Minder policy core: owner-managed limits, allowlists and a time-boxed agent
//! mandate, with a single pure evaluation function. This module moves no value
//! and signs nothing.

pub mod policy;
pub mod types;
pub mod window;

use near_sdk::json_types::{U128, U64};
use near_sdk::store::{IterableMap, IterableSet, LookupMap};
use near_sdk::{env, near, require, AccountId, BorshStorageKey, PanicOnDefault};

pub use types::{AssetId, AssetPolicy, Config, Decision, DenyReason, Mandate, Status};

use policy::EvalInput;
use window::SpendRing;

/// Bounds the ring stored per asset and the work done per evaluation.
const MAX_NUM_BUCKETS: u32 = 64;
const MAX_PAGE_SIZE: u32 = 100;

#[near(serializers = [borsh])]
#[derive(BorshStorageKey)]
enum StorageKey {
    Policies,
    Recipients,
    Spend,
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

    // ---- Views ----

    /// Evaluates a hypothetical action as `caller` would experience it. A view
    /// has no authenticated caller, so the agent identity is an argument; the
    /// real action path will pass the predecessor account instead.
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

    // ---- Test support ----

    /// Records spend into the current slot. Compiled only with the
    /// `test-helpers` feature; stands in for the settle step added later.
    #[cfg(feature = "test-helpers")]
    pub fn test_record_spend(&mut self, asset: AssetId, amount: U128) {
        self.assert_owner();
        let (now, slot_len) = (env::block_timestamp(), self.slot_len());
        let num_buckets = self.config.num_buckets;
        self.spend
            .entry(asset)
            .or_insert_with(|| SpendRing::new(num_buckets))
            .record(now, slot_len, amount.0);
    }
}

impl Minder {
    fn assert_owner(&self) {
        require!(
            env::predecessor_account_id() == self.config.owner,
            "Only the owner can call this method"
        );
    }

    fn slot_len(&self) -> u64 {
        self.config.window_len.0 / u64::from(self.config.num_buckets)
    }

    fn live_spend(&self, asset: &AssetId) -> u128 {
        self.spend.get(asset).map_or(0, |ring| {
            ring.live_sum(env::block_timestamp(), self.slot_len())
        })
    }

    /// Gathers state and defers every decision to [`policy::evaluate`].
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
            live_spend: self.live_spend(asset),
        })
    }
}

fn page(from_index: Option<u32>, limit: Option<u32>) -> (usize, usize) {
    let skip = from_index.unwrap_or(0) as usize;
    let take = limit.unwrap_or(MAX_PAGE_SIZE).min(MAX_PAGE_SIZE) as usize;
    (skip, take)
}
