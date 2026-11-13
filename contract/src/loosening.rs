//! The tighten-versus-loosen classification of an owner change: the one
//! function that decides whether a change takes effect at once or waits out the
//! timelock. Pure, like `policy::evaluate`: it performs no I/O and reads no
//! contract state itself. Everything it needs it asks of a [`LooseningState`],
//! which the contract implements over its storage (see `timelock.rs`) and the
//! Kani proofs implement over plain symbolic data, so what is proved is this
//! file as it ships.
//!
//! Every comparison and boundary lives here. Only data fetching (a set lookup,
//! a stored value, parsing an address) is the state's job.
//!
//! The dangerous failure is a real loosening classified as not loosening: it
//! would take effect at once and bypass the timelock. See DESIGN.md, "Formal
//! verification", for what is proved of it.

use near_sdk::json_types::U128;
use near_sdk::AccountId;

use crate::types::{
    AssetId, AssetPolicy, OwnerChange, Status, DEFAULT_GAS_LIMIT, DEFAULT_TOKEN_GAS_LIMIT,
};

/// What the classifier needs to know of the live mandate: whether it was
/// revoked and when it expires (ns). Its key and scope play no part: a grant is
/// classified by time and budget alone (see DESIGN.md, "Owner timelock").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MandateTiming {
    pub revoked: bool,
    pub expiry: u64,
}

/// The bounds of the stored EVM configuration that a new configuration is
/// compared with. The signer, derivation path and sender are fixed once set, so
/// they cannot differ and are not compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvmBounds {
    pub chain_id: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    pub gas_limit: u64,
    pub token_gas_limit: u64,
}

/// The state a change is classified against. Reads are lazy (a method is
/// called only when the variant under classification needs it), so a
/// classification costs no more storage reads than before it was extracted.
pub trait LooseningState {
    /// Current block time, in nanoseconds.
    fn now(&self) -> u64;
    fn status(&self) -> Status;
    /// The policy stored for `asset`, if any.
    fn policy_of(&self, asset: &AssetId) -> Option<AssetPolicy>;
    /// Whether `recipient` is on the NEAR allowlist.
    fn has_recipient(&self, recipient: &AccountId) -> bool;
    /// Whether the EVM address written as `address` is on the EVM allowlist.
    fn has_evm_recipient(&self, address: &str) -> bool;
    /// The current mandate, if there is one (live, revoked or expired).
    fn mandate_timing(&self) -> Option<MandateTiming>;
    /// Whether any spend has been booked in the current budget period.
    fn budget_period_used(&self) -> bool;
    /// The stored EVM configuration, if any.
    fn evm_bounds(&self) -> Option<EvmBounds>;
    /// The timelock delay, in nanoseconds.
    fn delay(&self) -> u64;
    fn guardian(&self) -> Option<AccountId>;
    fn near_treasury(&self) -> Option<AccountId>;
    /// The registered EVM treasury, as the 20 bytes of its stored (normalized)
    /// address.
    fn evm_treasury(&self) -> Option<[u8; 20]>;
    /// This contract's own derived EVM address, if EVM payments are configured.
    fn evm_sender(&self) -> Option<[u8; 20]>;
    /// The 20 bytes of an address written as text. Panics on invalid text.
    fn parse_evm_address(&self, text: &str) -> [u8; 20];
    /// The 20 bytes of a proposed EVM treasury written as text. Panics on
    /// invalid text and on the zero address.
    fn parse_evm_treasury(&self, text: &str) -> [u8; 20];
}

/// Whether the change would loosen authority, given the state it would
/// change. Equal counts as not loosening.
pub fn loosens<S: LooseningState>(change: &OwnerChange, state: &S) -> bool {
    match change {
        OwnerChange::SetAssetPolicy {
            asset,
            per_tx_cap,
            window_limit,
            epoch_budget,
        } => match state.policy_of(asset) {
            // Adding an asset adds authority.
            None => true,
            Some(old) => policy_loosens(&old, *per_tx_cap, *window_limit, *epoch_budget),
        },
        OwnerChange::SetRecipient { recipient, allowed } => {
            *allowed && !state.has_recipient(recipient)
        }
        OwnerChange::SetEvmRecipient { address, allowed } => {
            *allowed && !state.has_evm_recipient(address)
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
            let extends_a_live_mandate = match state.mandate_timing() {
                Some(live) if !live.revoked && state.now() < live.expiry => expiry.0 > live.expiry,
                _ => false,
            };
            extends_a_live_mandate || (reset_budget.unwrap_or(false) && state.budget_period_used())
        }
        OwnerChange::Unfreeze => state.status() == Status::Frozen,
        OwnerChange::SetEvmConfig {
            chain_id,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            gas_limit,
            token_gas_limit,
            ..
        } => match state.evm_bounds() {
            // A first configuration is what makes EVM payments possible.
            None => true,
            Some(old) => {
                *chain_id != old.chain_id
                    || max_fee_per_gas.0 > old.max_fee_per_gas
                    || max_priority_fee_per_gas.0 > old.max_priority_fee_per_gas
                    || gas_limit.unwrap_or(DEFAULT_GAS_LIMIT) > old.gas_limit
                    || token_gas_limit.unwrap_or(DEFAULT_TOKEN_GAS_LIMIT) > old.token_gas_limit
            }
        },
        // A guardian is a stop-only role, so adding one enables no drain.
        // Removing one, or swapping it for another account, takes the
        // current reaction mechanism away.
        OwnerChange::SetGuardian { guardian } => {
            let current = state.guardian();
            current.is_some() && *guardian != current
        }
        OwnerChange::SetTimelockDelay { delay } => delay.0 < state.delay(),
        OwnerChange::SetTreasury { near, evm } => {
            *near != state.near_treasury()
                || evm.as_deref().map(|text| state.parse_evm_treasury(text)) != state.evm_treasury()
        }
        OwnerChange::OwnerWithdraw { to, .. } => state.near_treasury().as_ref() != Some(to),
        OwnerChange::OwnerEvmWithdraw { recipient, .. }
        | OwnerChange::OwnerEvmTokenWithdraw { recipient, .. } => !is_safe_destination(
            &state.parse_evm_address(recipient),
            state.evm_treasury().as_ref(),
            state.evm_sender().as_ref(),
        ),
    }
}

/// An EVM destination that moves nothing out: the treasury, or this
/// contract's own derived address (a self-transfer, which is how a stuck
/// or signed-but-unbroadcast transaction is cancelled).
///
/// Compared as the 20 address bytes. The contract compared the lowercase text
/// of the addresses before; the two agree because the text of an address is a
/// one-to-one function of its bytes and the stored treasury is always in that
/// normalized form.
pub fn is_safe_destination(
    destination: &[u8; 20],
    treasury: Option<&[u8; 20]>,
    sender: Option<&[u8; 20]>,
) -> bool {
    treasury == Some(destination) || sender == Some(destination)
}

/// Whether replacing `old` with the given fields loosens any of them. A cap or
/// a window that is higher loosens, and so does dropping a budget or raising
/// it; a budget where there was none tightens. One loosening field makes the
/// whole change loosening, whatever else it lowers.
pub fn policy_loosens(
    old: &AssetPolicy,
    per_tx_cap: U128,
    window_limit: U128,
    epoch_budget: Option<U128>,
) -> bool {
    let budget_loosens = match (old.epoch_budget, epoch_budget) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(old), Some(new)) => new.0 > old.0,
    };
    per_tx_cap.0 > old.per_tx_cap.0 || window_limit.0 > old.window_limit.0 || budget_loosens
}
