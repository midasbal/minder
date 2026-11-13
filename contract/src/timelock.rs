//! The asymmetric policy-change timelock: tightening and emergency actions take
//! effect at once, loosening ones wait out a delay, and owner recovery is
//! instant only to a pre-registered treasury.
//!
//! The classification is made by the contract, from the change and the state it
//! would change, not by separate methods. Each owner method builds the
//! [`OwnerChange`] it was asked for and asks [`Minder::owner_change`]: a change
//! that only tightens, or leaves authority equal, is applied; one that loosens
//! anything is refused with a pointer to `propose_change`, which records it with
//! a ready-at time `delay` after now. `execute_change` applies it once that time
//! has come, and `cancel_change` drops it. Proposing and executing are
//! owner-only; cancelling is also open to the guardian, except for a change to
//! the guardian itself. A delay of zero disables the timelock, and every change
//! is then instant.
//!
//! What counts as loosening (see `DESIGN.md` for the reasoning):
//! - a policy change that raises the cap or the window, drops or raises the
//!   budget, or adds the asset; a mixed change counts as loosening;
//! - adding a recipient (NEAR or EVM);
//! - unfreezing;
//! - a grant that extends a live mandate's expiry;
//! - a grant that resets the budget period while some of its budget is spent;
//! - an EVM configuration that is new, or raises a fee or a gas limit, or moves
//!   the chain;
//! - lowering the delay, and changing the treasury;
//! - removing or replacing the guardian (adding one is instant);
//! - owner recovery to any destination but the treasury of its surface or, for
//!   EVM, the contract's own derived address (a self-transfer moves nothing out
//!   and is how a stuck transaction is cancelled).
//!
//! Instant: `freeze`, `revoke_mandate`, removing an asset or a recipient, a
//! policy change that is tighter or equal in every field, a grant that does not
//! extend a live mandate, raising the delay, and recovery to the treasury.

use near_sdk::json_types::U64;
use near_sdk::serde_json::json;
use near_sdk::{env, near, require, AccountId, Promise, PromiseOrValue};

use crate::evm;
use crate::types::{AssetPolicy, OwnerChange, PendingChange, Status, DEFAULT_TOKEN_GAS_LIMIT};
use crate::{page, Minder, MinderExt};

/// Bounds the storage a (compromised) owner key can fill with proposals.
const MAX_PENDING_CHANGES: u32 = 32;

/// Unwraps the promise of a recovery method, which always has one.
pub(crate) trait IntoPromise {
    fn into_promise(self) -> Promise;
}

impl IntoPromise for PromiseOrValue<()> {
    fn into_promise(self) -> Promise {
        match self {
            PromiseOrValue::Promise(promise) => promise,
            PromiseOrValue::Value(()) => env::panic_str("this change moves no funds"),
        }
    }
}

#[near]
impl Minder {
    /// Records a change that loosens authority, executable `timelock_delay`
    /// after now. Refuses a change that is not timelocked (call its method
    /// directly) and one that is invalid. Returns the change's id, which names
    /// it for `execute_change` and `cancel_change`. Owner only.
    pub fn propose_change(&mut self, change: OwnerChange) -> U64 {
        self.assert_owner();
        self.validate_change(&change);
        require!(
            self.is_timelocked(&change),
            "Not timelocked: this change does not loosen anything, or no timelock is set. Call its method directly"
        );
        require!(
            self.pending_changes.len() < MAX_PENDING_CHANGES,
            "Too many pending changes: execute or cancel some first"
        );
        let id = self.next_change_id;
        self.next_change_id += 1;
        let now = env::block_timestamp();
        let pending = PendingChange {
            id: U64(id),
            change,
            proposed_at: U64(now),
            ready_at: U64(now.saturating_add(self.config.timelock_delay.0)),
        };
        log_change("owner_change_proposed", &pending);
        self.pending_changes.insert(id, pending);
        U64(id)
    }

    /// Applies a pending change whose delay has passed. A change that cannot
    /// be applied now (for instance a grant whose expiry has since passed)
    /// reverts and stays pending, to be cancelled. Owner only.
    pub fn execute_change(&mut self, id: U64) -> PromiseOrValue<()> {
        self.assert_owner();
        let pending = self
            .pending_changes
            .get(&id.0)
            .cloned()
            .unwrap_or_else(|| env::panic_str("No such pending change"));
        require!(
            env::block_timestamp() >= pending.ready_at.0,
            "Not ready: the delay has not passed"
        );
        // Applied before the proposal is dropped: if it panics the whole call
        // reverts and the proposal stays, to be fixed or cancelled.
        let result = self.apply_change(pending.change.clone());
        self.pending_changes.remove(&id.0);
        log_change("owner_change_executed", &pending);
        result
    }

    /// Drops a pending change. Always available to the owner, whatever else
    /// has happened. The guardian may cancel too, except a change that concerns
    /// the guardian itself (so a compromised guardian cannot hold off its own
    /// removal).
    pub fn cancel_change(&mut self, id: U64) {
        self.assert_owner_or_guardian();
        let guardian_call = env::predecessor_account_id() != self.config.owner;
        let pending = self
            .pending_changes
            .get(&id.0)
            .cloned()
            .unwrap_or_else(|| env::panic_str("No such pending change"));
        require!(
            !(guardian_call && matches!(pending.change, OwnerChange::SetGuardian { .. })),
            "The guardian cannot cancel a change to the guardian"
        );
        self.pending_changes.remove(&id.0);
        log_change("owner_change_cancelled", &pending);
    }

    /// Sets or clears the guardian. Adding one (none to some) takes effect at
    /// once, since a guardian can only stop the contract. Removing or replacing
    /// one is a loosening change, because it takes the reaction mechanism away,
    /// and is timelocked.
    pub fn set_guardian(&mut self, guardian: Option<AccountId>) {
        self.assert_owner();
        self.owner_change(OwnerChange::SetGuardian { guardian });
    }

    /// Sets the delay of the timelock. Raising it takes effect at once;
    /// lowering it is a loosening change and is timelocked. Zero disables the
    /// timelock.
    pub fn set_timelock_delay(&mut self, delay: U64) {
        self.assert_owner();
        self.owner_change(OwnerChange::SetTimelockDelay { delay });
    }

    /// Registers the destinations of instant owner recovery: a NEAR account
    /// and an EVM address (`None` leaves a surface with no treasury, so all its
    /// recovery is timelocked). Changing the treasury is a loosening change.
    pub fn set_treasury(&mut self, near: Option<AccountId>, evm: Option<String>) {
        self.assert_owner();
        self.owner_change(OwnerChange::SetTreasury { near, evm });
    }

    pub fn get_pending_change(&self, id: U64) -> Option<PendingChange> {
        self.pending_changes.get(&id.0).cloned()
    }

    /// Pending changes in id order.
    pub fn get_pending_changes(
        &self,
        from_index: Option<u32>,
        limit: Option<u32>,
    ) -> Vec<PendingChange> {
        let mut all: Vec<PendingChange> = self
            .pending_changes
            .iter()
            .map(|(_, pending)| pending.clone())
            .collect();
        all.sort_by_key(|pending| pending.id.0);
        let (skip, take) = page(from_index, limit);
        all.into_iter().skip(skip).take(take).collect()
    }
}

impl Minder {
    /// The one entry for an owner change that may loosen: validate it, refuse
    /// it if it is timelocked, otherwise apply it. The caller has checked the
    /// owner.
    pub(crate) fn owner_change(&mut self, change: OwnerChange) {
        let _ = self.checked_owner_change(change);
    }

    /// `owner_change` for a recovery method, which returns the transfer's promise.
    pub(crate) fn owner_withdrawal(&mut self, change: OwnerChange) -> Promise {
        self.checked_owner_change(change).into_promise()
    }

    fn checked_owner_change(&mut self, change: OwnerChange) -> PromiseOrValue<()> {
        self.validate_change(&change);
        require!(
            !self.is_timelocked(&change),
            "Timelocked: this change loosens authority, so it takes effect only through propose_change and, after the delay, execute_change"
        );
        self.apply_change(change)
    }

    fn is_timelocked(&self, change: &OwnerChange) -> bool {
        self.config.timelock_delay.0 > 0 && self.is_loosening(change)
    }

    /// Checks everything about a change that does not depend on timing, so a
    /// malformed change is refused when proposed, not when it comes due.
    fn validate_change(&self, change: &OwnerChange) {
        match change {
            OwnerChange::SetEvmRecipient { address, .. } => {
                crate::gate::parse_address(address);
            }
            OwnerChange::GrantMandate { expiry, scope, .. } => {
                require!(
                    expiry.0 > env::block_timestamp(),
                    "Mandate expiry must be in the future"
                );
                if let Some(scope) = scope {
                    scope
                        .clone()
                        .validated()
                        .unwrap_or_else(|reason| env::panic_str(reason));
                }
            }
            OwnerChange::SetEvmConfig {
                signer_id,
                derivation_path,
                sender,
                chain_id,
                max_fee_per_gas,
                max_priority_fee_per_gas,
                gas_limit,
                token_gas_limit,
            } => {
                Self::build_evm_config(
                    signer_id.clone(),
                    derivation_path.clone(),
                    sender,
                    *chain_id,
                    *max_fee_per_gas,
                    *max_priority_fee_per_gas,
                    *gas_limit,
                    *token_gas_limit,
                );
            }
            OwnerChange::SetTreasury { evm, .. } => {
                if let Some(evm) = evm {
                    normalized_treasury(evm);
                }
            }
            OwnerChange::SetGuardian { guardian } => {
                if let Some(guardian) = guardian {
                    Self::require_valid_guardian(guardian, &self.config.owner);
                }
            }
            OwnerChange::OwnerWithdraw { asset, .. } => Self::require_local(asset),
            OwnerChange::OwnerEvmWithdraw {
                recipient, nonce, ..
            } => {
                crate::gate::parse_address(recipient);
                require!(nonce.0 < u64::MAX, "The nonce space is exhausted");
            }
            OwnerChange::OwnerEvmTokenWithdraw {
                token,
                recipient,
                nonce,
                ..
            } => {
                crate::gate::parse_address(token);
                crate::gate::parse_address(recipient);
                require!(nonce.0 < u64::MAX, "The nonce space is exhausted");
            }
            OwnerChange::SetAssetPolicy { .. }
            | OwnerChange::SetRecipient { .. }
            | OwnerChange::Unfreeze
            | OwnerChange::SetTimelockDelay { .. } => {}
        }
    }

    /// Whether the change would loosen authority, given the state it would
    /// change. Equal counts as not loosening.
    fn is_loosening(&self, change: &OwnerChange) -> bool {
        match change {
            OwnerChange::SetAssetPolicy {
                asset,
                per_tx_cap,
                window_limit,
                epoch_budget,
            } => match self.policies.get(asset) {
                // Adding an asset adds authority.
                None => true,
                Some(old) => policy_loosens(old, *per_tx_cap, *window_limit, *epoch_budget),
            },
            OwnerChange::SetRecipient { recipient, allowed } => {
                *allowed && !self.recipients.contains(recipient)
            }
            OwnerChange::SetEvmRecipient { address, allowed } => {
                *allowed
                    && !self
                        .evm_recipients
                        .contains(&crate::gate::parse_address(address))
            }
            // Only a grant that extends a mandate that is live loosens: any
            // other grant (none, revoked, expired, or no later expiry) leaves
            // the agent bounded by the same policy for no longer than before.
            // A grant that asks for a fresh budget period also loosens when
            // some of the period's budget has been spent: it hands that back.
            // With nothing spent it changes nothing.
            OwnerChange::GrantMandate {
                expiry,
                reset_budget,
                ..
            } => {
                let extends_a_live_mandate = match &self.mandate {
                    Some(live) if !live.revoked && env::block_timestamp() < live.expiry.0 => {
                        expiry.0 > live.expiry.0
                    }
                    _ => false,
                };
                extends_a_live_mandate || (reset_budget.unwrap_or(false) && self.budget_period_used)
            }
            OwnerChange::Unfreeze => self.config.status == Status::Frozen,
            OwnerChange::SetEvmConfig {
                chain_id,
                max_fee_per_gas,
                max_priority_fee_per_gas,
                gas_limit,
                token_gas_limit,
                ..
            } => match &self.evm_config {
                // A first configuration is what makes EVM payments possible.
                None => true,
                Some(old) => {
                    *chain_id != old.chain_id
                        || max_fee_per_gas.0 > old.max_fee_per_gas.0
                        || max_priority_fee_per_gas.0 > old.max_priority_fee_per_gas.0
                        || gas_limit.unwrap_or(crate::gate::DEFAULT_GAS_LIMIT) > old.gas_limit
                        || token_gas_limit.unwrap_or(DEFAULT_TOKEN_GAS_LIMIT) > old.token_gas_limit
                }
            },
            // A guardian is a stop-only role, so adding one enables no drain.
            // Removing one, or swapping it for another account, takes the
            // current reaction mechanism away.
            OwnerChange::SetGuardian { guardian } => {
                self.config.guardian.is_some() && *guardian != self.config.guardian
            }
            OwnerChange::SetTimelockDelay { delay } => delay.0 < self.config.timelock_delay.0,
            OwnerChange::SetTreasury { near, evm } => {
                *near != self.config.near_treasury
                    || evm.as_deref().map(normalized_treasury) != self.config.evm_treasury
            }
            OwnerChange::OwnerWithdraw { to, .. } => self.config.near_treasury.as_ref() != Some(to),
            OwnerChange::OwnerEvmWithdraw { recipient, .. }
            | OwnerChange::OwnerEvmTokenWithdraw { recipient, .. } => {
                !self.is_evm_safe_destination(recipient)
            }
        }
    }

    /// An EVM destination that moves nothing out: the treasury, or this
    /// contract's own derived address (a self-transfer, which is how a stuck
    /// or signed-but-unbroadcast transaction is cancelled).
    fn is_evm_safe_destination(&self, recipient: &str) -> bool {
        let destination = evm::format_address(&crate::gate::parse_address(recipient));
        self.config.evm_treasury.as_deref() == Some(destination.as_str())
            || self
                .evm_config
                .as_ref()
                .is_some_and(|config| evm::format_address(&config.sender) == destination)
    }

    /// Applies a change with no further checks of timing.
    pub(crate) fn apply_change(&mut self, change: OwnerChange) -> PromiseOrValue<()> {
        match change {
            OwnerChange::SetAssetPolicy {
                asset,
                per_tx_cap,
                window_limit,
                epoch_budget,
            } => self.apply_set_asset_policy(asset, per_tx_cap, window_limit, epoch_budget),
            OwnerChange::SetRecipient { recipient, allowed } => {
                self.apply_set_recipient(recipient, allowed)
            }
            OwnerChange::SetEvmRecipient { address, allowed } => {
                self.apply_set_evm_recipient(address, allowed)
            }
            OwnerChange::GrantMandate {
                agent_key,
                expiry,
                scope,
                reset_budget,
            } => self.apply_grant_mandate(agent_key, expiry, scope, reset_budget.unwrap_or(false)),
            OwnerChange::Unfreeze => self.apply_unfreeze(),
            OwnerChange::SetEvmConfig {
                signer_id,
                derivation_path,
                sender,
                chain_id,
                max_fee_per_gas,
                max_priority_fee_per_gas,
                gas_limit,
                token_gas_limit,
            } => self.apply_set_evm_config(
                signer_id,
                derivation_path,
                sender,
                chain_id,
                max_fee_per_gas,
                max_priority_fee_per_gas,
                gas_limit,
                token_gas_limit,
            ),
            OwnerChange::SetTimelockDelay { delay } => self.config.timelock_delay = delay,
            OwnerChange::SetGuardian { guardian } => self.config.guardian = guardian,
            OwnerChange::SetTreasury { near, evm } => {
                self.config.near_treasury = near;
                self.config.evm_treasury = evm.as_deref().map(normalized_treasury);
            }
            OwnerChange::OwnerWithdraw { asset, amount, to } => {
                return PromiseOrValue::Promise(self.apply_owner_withdraw(asset, amount, to));
            }
            OwnerChange::OwnerEvmWithdraw {
                recipient,
                value,
                nonce,
            } => {
                return PromiseOrValue::Promise(
                    self.apply_owner_evm_withdraw(recipient, value, nonce),
                );
            }
            OwnerChange::OwnerEvmTokenWithdraw {
                token,
                recipient,
                amount,
                nonce,
            } => {
                return PromiseOrValue::Promise(
                    self.apply_owner_evm_token_withdraw(token, recipient, amount, nonce),
                );
            }
        }
        PromiseOrValue::Value(())
    }
}

/// Whether replacing `old` with the given fields loosens any of them. A cap or
/// a window that is higher loosens, and so does dropping a budget or raising
/// it; a budget where there was none tightens. One loosening field makes the
/// whole change loosening, whatever else it lowers.
fn policy_loosens(
    old: &AssetPolicy,
    per_tx_cap: near_sdk::json_types::U128,
    window_limit: near_sdk::json_types::U128,
    epoch_budget: Option<near_sdk::json_types::U128>,
) -> bool {
    let budget_loosens = match (old.epoch_budget, epoch_budget) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(old), Some(new)) => new.0 > old.0,
    };
    per_tx_cap.0 > old.per_tx_cap.0 || window_limit.0 > old.window_limit.0 || budget_loosens
}

/// A treasury EVM address in its stored form: lowercase, and not the zero
/// address (which would burn what is sent to it).
pub(crate) fn normalized_treasury(address: &str) -> String {
    let bytes = crate::gate::parse_address(address);
    require!(
        bytes != [0u8; 20],
        "The treasury must not be the zero address"
    );
    evm::format_address(&bytes)
}

fn log_change(event: &str, pending: &PendingChange) {
    env::log_str(&format!(
        "EVENT_JSON:{}",
        json!({
            "standard": "minder",
            "version": "1.0.0",
            "event": event,
            "data": [{
                "by": env::predecessor_account_id(),
                "id": pending.id,
                "ready_at": pending.ready_at,
                "change": pending.change,
            }],
        })
    ));
}
