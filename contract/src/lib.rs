//! Minder: owner-managed limits, allowlists and a time-boxed agent mandate,
//! enforced by a single pure evaluation function. The agent moves value on NEAR
//! only through `request_local_transfer`, which reserves budget before the
//! transfer and settles it in a callback. Nothing here signs for other chains.

mod decision;
pub mod evm;
mod gate;
pub mod policy;
mod timelock;
pub mod types;
pub mod window;

use near_sdk::json_types::{U128, U64};
use near_sdk::serde_json::json;
use near_sdk::store::{IterableMap, IterableSet, LookupMap};
use near_sdk::{
    env, near, require, AccountId, Allowance, BorshStorageKey, Gas, NearToken, PanicOnDefault,
    Promise, PromiseError, PublicKey,
};

pub use types::{
    AssetId, AssetPolicy, Config, Decision, DenyReason, EpochSpend, EvmConfig, EvmConfigView,
    Mandate, MandateScope, OwnerChange, PendingChange, Status, TokenPaymentDecision,
};

use policy::{EvalInput, RecipientRef};
use window::SpendRing;

/// Bounds the ring stored per asset and the work done per evaluation.
const MAX_NUM_BUCKETS: u32 = 64;
const MAX_PAGE_SIZE: u32 = 100;

/// Gas for the token's `ft_transfer`. Gas for the settlement callback is
/// reserved separately and up front: if the callback could run out of gas the
/// reservation would never be released.
const FT_TRANSFER_GAS: Gas = Gas::from_tgas(30);
const SETTLE_CALLBACK_GAS: Gas = Gas::from_tgas(15);

/// The only methods the agent's access key may call, fixed here and never taken
/// from a parameter. A function-call access key restricts the methods at the
/// protocol level, before any contract code runs; this list is what makes the
/// restriction exactly the three agent entry points. It is a comma-separated
/// list, as the runtime takes it. A sandbox test reads the key back from the
/// chain and compares it to this.
const AGENT_METHODS: &str = "request_local_transfer,request_evm_payment,request_evm_token_payment";

/// The gas allowance of the agent's access key (0.25 NEAR). It is
/// the most the key can spend on transaction fees in total, paid from this
/// contract's balance, so it counts toward the worst-case loss in addition to
/// the policy limits. A fixed constant, not a parameter: the agent cannot
/// influence it and the owner's exposure from it is bounded and documented.
const AGENT_KEY_ALLOWANCE: NearToken = NearToken::from_millinear(250);

const LOCAL_ONLY: &str =
    "EVM assets are paid through request_evm_payment and request_evm_token_payment";

#[near(serializers = [borsh])]
#[derive(BorshStorageKey)]
enum StorageKey {
    Policies,
    Recipients,
    Spend,
    Reserved,
    EvmRecipients,
    EvmNonces,
    EpochSpend,
    PendingChanges,
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
    /// Absent until the owner configures EVM payments.
    evm_config: Option<EvmConfig>,
    /// Allowed 20-byte EVM recipients; separate from the NEAR `recipients`.
    evm_recipients: IterableSet<[u8; 20]>,
    /// Next nonce to hand out per chain id. Only ever increases.
    evm_nonces: LookupMap<u64, u64>,
    /// Committed spend per asset under one mandate epoch, for the policy's
    /// `epoch_budget`. An entry from an older epoch than the current mandate's
    /// reads as zero (see [`Self::epoch_committed`]). Like `spend`, it is kept
    /// when a policy is removed, so removing and re-adding a policy cannot
    /// reset a budget.
    epoch_spend: LookupMap<AssetId, EpochSpend>,
    /// Loosening changes waiting out the timelock, by id.
    pending_changes: IterableMap<u64, PendingChange>,
    /// The id the next proposal gets. Starts at 1 and never decreases, so an id
    /// is never reused.
    next_change_id: u64,
}

#[near]
impl Minder {
    /// Single-use: `#[init]` panics if state already exists. The caller must
    /// be the designated owner or the contract account itself, so that nobody
    /// else can claim ownership by initializing first. Deploy and initialize
    /// in one batched transaction.
    ///
    /// `timelock_delay` (ns; omitted or zero disables the timelock) and the
    /// treasuries (`near_treasury`, `evm_treasury`, the destinations of instant
    /// owner recovery) are the asymmetric-timelock settings; see `timelock.rs`.
    /// `guardian` is the optional stop-only second account (see
    /// [`Config::guardian`]); it must be neither the owner nor this contract.
    #[init]
    #[allow(clippy::too_many_arguments)]
    pub fn init(
        owner: AccountId,
        window_len: U64,
        num_buckets: u32,
        timelock_delay: Option<U64>,
        near_treasury: Option<AccountId>,
        evm_treasury: Option<String>,
        guardian: Option<AccountId>,
    ) -> Self {
        let caller = env::predecessor_account_id();
        require!(
            caller == owner || caller == env::current_account_id(),
            "init must be called by the owner or the contract account"
        );
        // The agent's key lives on this contract's account. The owner must be a
        // different account, so that the contract account's full-access key can
        // be removed (see DESIGN.md) without losing owner control.
        require!(
            owner != env::current_account_id(),
            "The owner must be a separate account from the contract account"
        );
        if let Some(guardian) = &guardian {
            Self::require_valid_guardian(guardian, &owner);
        }
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
                timelock_delay: timelock_delay.unwrap_or(U64(0)),
                near_treasury,
                evm_treasury: evm_treasury.as_deref().map(timelock::normalized_treasury),
                guardian,
            },
            mandate: None,
            last_mandate_epoch: 0,
            policies: IterableMap::new(StorageKey::Policies),
            recipients: IterableSet::new(StorageKey::Recipients),
            spend: LookupMap::new(StorageKey::Spend),
            reserved: LookupMap::new(StorageKey::Reserved),
            evm_config: None,
            evm_recipients: IterableSet::new(StorageKey::EvmRecipients),
            evm_nonces: LookupMap::new(StorageKey::EvmNonces),
            epoch_spend: LookupMap::new(StorageKey::EpochSpend),
            pending_changes: IterableMap::new(StorageKey::PendingChanges),
            next_change_id: 1,
        }
    }

    // ---- Owner-only ----

    /// Creates or replaces the policy for `asset`, which also allowlists it.
    /// `epoch_budget` is an optional lifetime budget per mandate grant (see
    /// [`AssetPolicy::epoch_budget`]); omit it for the window alone. Spend is
    /// tracked per epoch whether or not a budget is set, so a budget set
    /// mid-grant counts what that grant already spent.
    pub fn set_asset_policy(
        &mut self,
        asset: AssetId,
        per_tx_cap: U128,
        window_limit: U128,
        epoch_budget: Option<U128>,
    ) {
        self.assert_owner();
        self.owner_change(OwnerChange::SetAssetPolicy {
            asset,
            per_tx_cap,
            window_limit,
            epoch_budget,
        });
    }

    /// Removes the policy, which un-allowlists the asset. Reverts if absent.
    pub fn remove_asset_policy(&mut self, asset: AssetId) {
        self.assert_owner();
        self.apply_remove_asset_policy(asset);
    }

    pub fn set_recipient(&mut self, recipient: AccountId, allowed: bool) {
        self.assert_owner();
        self.owner_change(OwnerChange::SetRecipient { recipient, allowed });
    }

    /// Replaces any existing mandate with a new one under a fresh epoch, and
    /// provisions `agent_key` as a function-call access key on this contract's
    /// own account, restricted to the three agent methods and to a fixed gas
    /// allowance. This is the only way to restore authority after revocation or
    /// expiry.
    ///
    /// A mandate that is still live has its key deleted first, so a replaced
    /// agent keeps no key. Each key action is its own promise, not one batch:
    /// the actions of a batch succeed or fail together, and a delete of a key
    /// that is already gone must not stop the new key from being added. The
    /// deletion is a cleanup; the new `agent_key` in the mandate is what
    /// authenticates, so a key left behind by a failed deletion cannot act.
    ///
    /// The provisioning is asynchronous: the mandate is recorded now and the
    /// key appears on the account in a later block. Until then nothing can
    /// authenticate as the agent, so the delay only withholds authority.
    pub fn grant_mandate(
        &mut self,
        agent_key: PublicKey,
        expiry: U64,
        scope: Option<MandateScope>,
    ) {
        self.assert_owner();
        self.owner_change(OwnerChange::GrantMandate {
            agent_key,
            expiry,
            scope,
        });
    }

    /// Reverts if there is no mandate. Revoking an already revoked mandate is
    /// a no-op.
    ///
    /// Two layers, both applied here. The `revoked` flag is the synchronous
    /// guard: `evaluate` denies from the next call on, whether or not the key
    /// deletion has landed, and even if it fails. The deletion of the access
    /// key is the protocol-level removal, as its own promise; if the key is
    /// already gone it fails without effect, and this call has already
    /// succeeded, so it cannot panic.
    pub fn revoke_mandate(&mut self) {
        self.assert_owner_or_guardian();
        self.apply_revoke_mandate();
    }

    pub fn freeze(&mut self) {
        self.assert_owner_or_guardian();
        self.apply_freeze();
    }

    pub fn unfreeze(&mut self) {
        self.assert_owner();
        self.owner_change(OwnerChange::Unfreeze);
    }

    /// Sends `amount` of `asset` from this contract to `to`. Owner recovery:
    /// it works in any status, including `Frozen`, and is deliberately outside
    /// policy. It checks no cap or allowlist, and neither reads nor changes
    /// `reserved` or the spend window. No callback: a failed transfer fails
    /// the returned promise and there is no accounting to undo. Minder contains
    /// the agent, not the owner.
    pub fn owner_withdraw(&mut self, asset: AssetId, amount: U128, to: AccountId) -> Promise {
        self.assert_owner();
        self.owner_withdrawal(OwnerChange::OwnerWithdraw { asset, amount, to })
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
        Self::require_local(&asset);
        let caller_key = Self::authenticated_key();
        let decision = self.evaluate_request(caller_key.as_ref(), &asset, amount.0, &recipient);
        // The decision event comes first, so a deny leaves it in the failed
        // receipt's logs before the revert.
        decision::log(
            &decision::Request {
                method: "request_local_transfer",
                agent_key: caller_key.as_ref(),
                asset: &asset,
                recipient: recipient.to_string(),
                amount: amount.0,
            },
            &[decision::Leg {
                leg: "transfer",
                asset: &asset,
                amount: amount.0,
                decision,
            }],
        );
        if let Decision::Deny(reason) = decision {
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

    /// Evaluates a hypothetical action as the holder of `agent_key` would
    /// experience it (signing a call on this contract's own account), counting
    /// in-flight reservations. A view has no signer, so the key is an argument;
    /// `request_local_transfer` uses the key that signed the call instead.
    pub fn dry_run(
        &self,
        agent_key: PublicKey,
        asset: AssetId,
        amount: U128,
        recipient: AccountId,
    ) -> Decision {
        Self::require_local(&asset);
        self.evaluate_request(Some(&agent_key), &asset, amount.0, &recipient)
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
}

/// Test support, compiled only with the `test-helpers` feature.
///
/// This is an impl block of its own, gated as a whole, so that in a production
/// build `#[near]` never sees it. Gating only the method inside the main block
/// would drop it from the wasm but not from the ABI: the macro emits an ABI
/// entry for a method whatever `#[cfg]` is on it.
#[cfg(feature = "test-helpers")]
#[near]
impl Minder {
    /// Seeds committed spend without moving value.
    pub fn test_record_spend(&mut self, asset: AssetId, amount: U128) {
        self.assert_owner();
        self.record_spend(&asset, amount.0);
    }
}

impl Minder {
    pub(crate) fn apply_set_asset_policy(
        &mut self,
        asset: AssetId,
        per_tx_cap: U128,
        window_limit: U128,
        epoch_budget: Option<U128>,
    ) {
        self.policies.insert(
            asset,
            AssetPolicy {
                per_tx_cap,
                window_limit,
                epoch_budget,
            },
        );
    }

    pub(crate) fn apply_remove_asset_policy(&mut self, asset: AssetId) {
        require!(
            self.policies.remove(&asset).is_some(),
            "No policy for this asset"
        );
    }

    pub(crate) fn apply_set_recipient(&mut self, recipient: AccountId, allowed: bool) {
        if allowed {
            self.recipients.insert(recipient);
        } else {
            self.recipients.remove(&recipient);
        }
    }

    pub(crate) fn apply_grant_mandate(
        &mut self,
        agent_key: PublicKey,
        expiry: U64,
        scope: Option<MandateScope>,
    ) {
        require!(
            expiry.0 > env::block_timestamp(),
            "Mandate expiry must be in the future"
        );
        // Unset means inherit: no scope is the unrestricted (global-policy-only)
        // mandate, as before scopes existed. A re-grant replaces the scope.
        let scope = scope
            .unwrap_or_default()
            .validated()
            .unwrap_or_else(|reason| env::panic_str(reason));
        if let Some(previous) = self.mandate.as_ref().filter(|mandate| !mandate.revoked) {
            // The same key would race its own deletion against its re-addition.
            // After a revocation the deletion has long landed, so reuse is fine.
            require!(
                previous.agent_key != agent_key,
                "That key is the current agent key; revoke the mandate first or use a new key"
            );
            Self::delete_agent_key(previous.agent_key.clone());
        }
        Promise::new(env::current_account_id())
            .add_access_key_allowance(
                agent_key.clone(),
                Allowance::limited(AGENT_KEY_ALLOWANCE)
                    .unwrap_or_else(|| env::panic_str("the allowance is non-zero")),
                env::current_account_id(),
                AGENT_METHODS,
            )
            .detach();
        self.last_mandate_epoch += 1;
        self.mandate = Some(Mandate {
            agent_key,
            expiry,
            epoch: self.last_mandate_epoch,
            revoked: false,
            scope,
        });
    }

    pub(crate) fn apply_revoke_mandate(&mut self) {
        let mandate = self.mandate.as_mut().unwrap_or_else(|| {
            env::panic_str("No mandate to revoke");
        });
        if !mandate.revoked {
            mandate.revoked = true;
            Self::delete_agent_key(mandate.agent_key.clone());
        }
    }

    pub(crate) fn apply_freeze(&mut self) {
        self.config.status = Status::Frozen;
    }

    pub(crate) fn apply_unfreeze(&mut self) {
        self.config.status = Status::Active;
    }

    pub(crate) fn apply_owner_withdraw(
        &mut self,
        asset: AssetId,
        amount: U128,
        to: AccountId,
    ) -> Promise {
        Self::require_local(&asset);
        Self::transfer_promise(&asset, amount, to)
    }

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
            AssetId::Evm(_) | AssetId::Evm20 { .. } => env::panic_str(LOCAL_ONLY),
        }
    }

    /// The NEAR entry points handle NEAR and NEP-141 assets only; EVM assets
    /// (native ETH and ERC-20 tokens) have no NEAR recipient and are paid
    /// through `request_evm_payment` and `request_evm_token_payment`.
    fn require_local(asset: &AssetId) {
        require!(!asset.is_evm(), LOCAL_ONLY);
    }

    /// The key that signed this call, if the call reached this contract from
    /// its own account; `None` for a call from any other account.
    ///
    /// The agent holds no account. It holds a function-call access key on this
    /// contract's account, so its transactions have this contract as both
    /// signer and predecessor, and the key is what tells them apart: the
    /// contract's own callbacks and any other key on the account carry a
    /// different (or no) key and never match the mandate's.
    pub(crate) fn authenticated_key() -> Option<PublicKey> {
        (env::predecessor_account_id() == env::current_account_id()).then(env::signer_account_pk)
    }

    /// Removes the agent's access key from this account, as a promise of its
    /// own. Deliberately not awaited: if the key is already gone the deletion
    /// fails, which changes nothing, and must not fail the call that issued it.
    fn delete_agent_key(agent_key: PublicKey) {
        Promise::new(env::current_account_id())
            .delete_key(agent_key)
            .detach();
    }

    pub(crate) fn assert_owner(&self) {
        require!(
            env::predecessor_account_id() == self.config.owner,
            "Only the owner can call this method"
        );
    }

    /// The three stop-powers (`freeze`, `revoke_mandate`, `cancel_change`) are
    /// open to the guardian as well as the owner. Nothing else is.
    pub(crate) fn assert_owner_or_guardian(&self) {
        let caller = env::predecessor_account_id();
        require!(
            caller == self.config.owner || self.config.guardian.as_ref() == Some(&caller),
            "Only the owner or the guardian can call this method"
        );
    }

    /// A guardian is a separate account from the owner (otherwise it adds
    /// nothing) and from this contract (whose own calls carry the agent's key).
    pub(crate) fn require_valid_guardian(guardian: &AccountId, owner: &AccountId) {
        require!(
            guardian != owner,
            "The guardian must be a different account from the owner"
        );
        require!(
            *guardian != env::current_account_id(),
            "The guardian must be a different account from the contract account"
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

        // And into the budget counter of the current mandate epoch. This is a
        // settlement under whatever grant is current now: spend that was
        // reserved under an earlier grant and settles after a re-grant is
        // charged to the new one, where it was already being counted as
        // in flight. That can only tighten the new grant, never loosen it.
        let committed = self.epoch_committed(asset).saturating_add(amount);
        self.epoch_spend.insert(
            asset.clone(),
            EpochSpend {
                epoch: self.current_epoch(),
                committed,
            },
        );
    }

    /// The epoch of the current mandate. Epochs start at 1, so 0 means no
    /// mandate was ever granted.
    fn current_epoch(&self) -> u64 {
        self.mandate.as_ref().map_or(0, |mandate| mandate.epoch)
    }

    /// Committed spend of `asset` under the current mandate epoch: the stored
    /// amount if it belongs to this epoch, otherwise zero. The lazy reset: a
    /// new grant bumps the epoch, and every older entry then reads as zero
    /// with nothing written.
    pub(crate) fn epoch_committed(&self, asset: &AssetId) -> u128 {
        match self.epoch_spend.get(asset) {
            Some(spend) if spend.epoch == self.current_epoch() => spend.committed,
            _ => 0,
        }
    }

    /// Evaluation for the NEAR recipient allowlist. Used by both `dry_run` and
    /// `request_local_transfer`.
    fn evaluate_request(
        &self,
        caller_key: Option<&PublicKey>,
        asset: &AssetId,
        amount: u128,
        recipient: &AccountId,
    ) -> Decision {
        self.evaluate_with(
            caller_key,
            asset,
            amount,
            RecipientRef::Near(recipient),
            self.recipients.contains(recipient),
        )
    }

    /// Gathers state and defers every decision to [`policy::evaluate`]. The
    /// one place an `EvalInput` is built (with [`Self::evaluate_gas`], which
    /// differs only in the rules it applies), whichever allowlist the
    /// recipient came from.
    fn evaluate_with(
        &self,
        caller_key: Option<&PublicKey>,
        asset: &AssetId,
        amount: u128,
        recipient: RecipientRef,
        recipient_allowed: bool,
    ) -> Decision {
        policy::evaluate(&self.eval_input(caller_key, asset, amount, recipient, recipient_allowed))
    }

    /// The gas leg of a token payment: `gas` wei against the native asset's
    /// policy. See [`policy::evaluate_gas`].
    fn evaluate_gas(
        &self,
        caller_key: Option<&PublicKey>,
        native: &AssetId,
        gas: u128,
    ) -> Decision {
        policy::evaluate_gas(&self.eval_input(caller_key, native, gas, RecipientRef::None, true))
    }

    fn eval_input<'a>(
        &'a self,
        caller_key: Option<&'a PublicKey>,
        asset: &'a AssetId,
        amount: u128,
        recipient: RecipientRef<'a>,
        recipient_allowed: bool,
    ) -> EvalInput<'a> {
        EvalInput {
            status: self.config.status,
            mandate: self.mandate.as_ref(),
            now: env::block_timestamp(),
            caller_key,
            asset,
            policy: self.policies.get(asset),
            recipient,
            recipient_allowed,
            amount,
            effective_spend: policy::effective_spend(
                self.live_spend(asset),
                self.reserved_of(asset),
            ),
            epoch_usage: policy::effective_spend(
                self.epoch_committed(asset),
                self.reserved_of(asset),
            ),
        }
    }
}

pub(crate) fn page(from_index: Option<u32>, limit: Option<u32>) -> (usize, usize) {
    let skip = from_index.unwrap_or(0) as usize;
    let take = limit.unwrap_or(MAX_PAGE_SIZE).min(MAX_PAGE_SIZE) as usize;
    (skip, take)
}

#[cfg(test)]
mod budget_tests;
#[cfg(test)]
mod custody_tests;
#[cfg(test)]
mod gate_tests;
#[cfg(test)]
mod guardian_tests;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod mandate_tests;
#[cfg(test)]
mod model_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod timelock_tests;
