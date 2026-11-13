//! Proofs of the timelock's classifier, `loosening::loosens`: for every owner
//! change and every state it is classified against, a change that really
//! loosens authority is never classified as not loosening (which would let it
//! take effect at once and bypass the timelock).
//!
//! The source is the contract's own `loosening.rs`, included by path. Each
//! variant of `OwnerChange` has two harnesses over the same symbolic case:
//!
//! - **soundness**, the one that matters: if the human-stated loosening spec
//!   holds, `loosens` returns true;
//! - **completeness**, the converse: if `loosens` returns true, the spec holds.
//!   Together they say `loosens` equals the spec for that variant.
//!
//! The specs are stated here in terms of what the change does to authority (a
//! request the policy newly admits, a recipient newly allowed, a time the agent
//! is newly authorized), not by copying the comparisons in the code. Where the
//! code is deliberately conservative (it calls more things loosening than a
//! strict reading would), the spec says so and completeness is stated against
//! that spec; see DESIGN.md.
//!
//! **What the symbolic state abstracts, exactly.** The classifier asks its
//! state only for numbers, flags, set membership (one element), equality of
//! optional accounts and equality of 20-byte addresses. The harness gives the
//! numbers, flags and bytes full symbolic range. A membership is one symbolic
//! bool, which is every answer a set could give for that one element, so the
//! set-valued dimensions (the recipient allowlists) are proved with no bound.
//! An optional account is drawn from `{none, a.near, b.near}`: the classifier
//! only compares accounts for equality, and two names of equal length make the
//! comparison a real byte comparison. Address text is not parsed here: the
//! state returns a symbolic 20 bytes for any text, so the proofs are about the
//! classification given what the parse returns (parsing is host-panicking code
//! covered by unit tests, and the contract's `Minder` does it).

use crate::loosening::{is_safe_destination, loosens, EvmBounds, LooseningState, MandateTiming};
use crate::policy::{evaluate, EvalInput, RecipientRef};
use crate::proofs::{key, key_equality};
use crate::types::{
    AssetId, AssetPolicy, Decision, Mandate, MandateScope, OwnerChange, Status, DEFAULT_GAS_LIMIT,
    DEFAULT_TOKEN_GAS_LIMIT,
};
use core::cmp::PartialEq;
use near_sdk::json_types::{U128, U64};
use near_sdk::{AccountId, PublicKey};

fn account_a() -> AccountId {
    AccountId::new_unvalidated("a.near".to_string())
}
fn account_b() -> AccountId {
    AccountId::new_unvalidated("b.near".to_string())
}

/// An optional account out of `{none, a.near, b.near}`.
fn any_account() -> Option<AccountId> {
    let which: u8 = kani::any();
    kani::assume(which <= 2);
    match which {
        0 => None,
        1 => Some(account_a()),
        _ => Some(account_b()),
    }
}

fn any_option<T>(value: impl FnOnce() -> T) -> Option<T> {
    if kani::any() {
        Some(value())
    } else {
        None
    }
}

fn any_status() -> Status {
    if kani::any() {
        Status::Frozen
    } else {
        Status::Active
    }
}

fn any_policy() -> AssetPolicy {
    AssetPolicy {
        per_tx_cap: U128(kani::any()),
        window_limit: U128(kani::any()),
        epoch_budget: any_option(|| U128(kani::any())),
    }
}

/// The state a change is classified against, every part symbolic.
struct State {
    now: u64,
    status: Status,
    policy: Option<AssetPolicy>,
    recipient_present: bool,
    evm_recipient_present: bool,
    mandate: Option<MandateTiming>,
    budget_period_used: bool,
    evm: Option<EvmBounds>,
    delay: u64,
    guardian: Option<AccountId>,
    near_treasury: Option<AccountId>,
    evm_treasury: Option<[u8; 20]>,
    evm_sender: Option<[u8; 20]>,
    /// What parsing any recipient text returns.
    parsed_address: [u8; 20],
    /// What parsing any proposed treasury text returns.
    parsed_treasury: [u8; 20],
}

impl State {
    fn any() -> Self {
        State {
            now: kani::any(),
            status: any_status(),
            policy: any_option(any_policy),
            recipient_present: kani::any(),
            evm_recipient_present: kani::any(),
            mandate: any_option(|| MandateTiming {
                revoked: kani::any(),
                expiry: kani::any(),
            }),
            budget_period_used: kani::any(),
            evm: any_option(|| EvmBounds {
                chain_id: kani::any(),
                max_fee_per_gas: kani::any(),
                max_priority_fee_per_gas: kani::any(),
                gas_limit: kani::any(),
                token_gas_limit: kani::any(),
            }),
            delay: kani::any(),
            guardian: any_account(),
            near_treasury: any_account(),
            evm_treasury: any_option(kani::any),
            evm_sender: any_option(kani::any),
            parsed_address: kani::any(),
            parsed_treasury: kani::any(),
        }
    }
}

impl LooseningState for State {
    fn now(&self) -> u64 {
        self.now
    }
    fn status(&self) -> Status {
        self.status
    }
    fn policy_of(&self, _asset: &AssetId) -> Option<AssetPolicy> {
        self.policy
    }
    fn has_recipient(&self, _recipient: &AccountId) -> bool {
        self.recipient_present
    }
    fn has_evm_recipient(&self, _address: &str) -> bool {
        self.evm_recipient_present
    }
    fn mandate_timing(&self) -> Option<MandateTiming> {
        self.mandate
    }
    fn budget_period_used(&self) -> bool {
        self.budget_period_used
    }
    fn evm_bounds(&self) -> Option<EvmBounds> {
        self.evm
    }
    fn delay(&self) -> u64 {
        self.delay
    }
    fn guardian(&self) -> Option<AccountId> {
        self.guardian.clone()
    }
    fn near_treasury(&self) -> Option<AccountId> {
        self.near_treasury.clone()
    }
    fn evm_treasury(&self) -> Option<[u8; 20]> {
        self.evm_treasury
    }
    fn evm_sender(&self) -> Option<[u8; 20]> {
        self.evm_sender
    }
    fn parse_evm_address(&self, _text: &str) -> [u8; 20] {
        self.parsed_address
    }
    fn parse_evm_treasury(&self, _text: &str) -> [u8; 20] {
        self.parsed_treasury
    }
}

/// A soundness and a completeness harness over one symbolic case. `$case`
/// returns `(actual, spec)`: what `loosens` says and what the spec says. Each
/// harness also covers both answers of `actual`, so neither is vacuous.
macro_rules! classifier_proofs {
    ($sound:ident, $complete:ident, $unwind:literal, $case:ident) => {
        #[kani::proof]
        #[kani::unwind($unwind)]
        fn $sound() {
            let (actual, spec) = $case();
            kani::cover!(actual);
            kani::cover!(!actual);
            if spec {
                assert!(actual, "a loosening was classified as not loosening");
            }
        }

        #[kani::proof]
        #[kani::unwind($unwind)]
        fn $complete() {
            let (actual, spec) = $case();
            if actual {
                assert!(
                    spec,
                    "a change was classified as loosening against the spec"
                );
            }
        }
    };
}

// ---- Recipients ----

/// Spec: a recipient is admitted after the change that was not before. The
/// change allows the address (so it is admitted after) or disallows it (so it
/// is not); it is admitted before iff it is already on the list.
fn recipient_case() -> (bool, bool) {
    let state = State::any();
    let allowed: bool = kani::any();
    let change = OwnerChange::SetRecipient {
        recipient: account_a(),
        allowed,
    };
    let admitted_after = allowed;
    let admitted_before = state.recipient_present;
    (loosens(&change, &state), admitted_after && !admitted_before)
}
classifier_proofs!(
    adding_a_near_recipient_is_never_instant,
    near_recipient_classification_is_exactly_the_spec,
    12,
    recipient_case
);

fn evm_recipient_case() -> (bool, bool) {
    let state = State::any();
    let allowed: bool = kani::any();
    let change = OwnerChange::SetEvmRecipient {
        address: "0x0000000000000000000000000000000000000001".to_string(),
        allowed,
    };
    let admitted_after = allowed;
    let admitted_before = state.evm_recipient_present;
    (loosens(&change, &state), admitted_after && !admitted_before)
}
classifier_proofs!(
    adding_an_evm_recipient_is_never_instant,
    evm_recipient_classification_is_exactly_the_spec,
    12,
    evm_recipient_case
);

// ---- Asset policy ----
//
// The oracle is the real `policy::evaluate`. Take any request and a state in
// which everything but the policy is in order (active contract, live mandate,
// the mandate's own key, an allowlisted recipient, no scope). If the new policy
// allows the request and the old one denies it, the change grants authority
// the firewall did not grant before, so it must be classified as loosening.
// No old policy at all is the same thing: the request is then denied for want
// of a policy.

fn decided(policy: Option<&AssetPolicy>, amount: u128, spend: u128, usage: u128) -> Decision {
    let mandate = Mandate {
        agent_key: key(1),
        expiry: U64(1),
        epoch: 1,
        revoked: false,
        scope: MandateScope::default(),
    };
    let caller = key(1);
    let bob = AccountId::new_unvalidated("bob.near".to_string());
    evaluate(&EvalInput {
        status: Status::Active,
        mandate: Some(&mandate),
        now: 0,
        caller_key: Some(&caller),
        asset: &AssetId::Near,
        policy,
        recipient: RecipientRef::Near(&bob),
        recipient_allowed: true,
        amount,
        effective_spend: spend,
        epoch_usage: usage,
    })
}

fn policy_change(new: &AssetPolicy) -> OwnerChange {
    OwnerChange::SetAssetPolicy {
        asset: AssetId::Near,
        per_tx_cap: new.per_tx_cap,
        window_limit: new.window_limit,
        epoch_budget: new.epoch_budget,
    }
}

/// A policy change that admits a request the old policy denied is never
/// classified as not loosening.
#[kani::proof]
#[kani::unwind(12)]
#[kani::stub(<PublicKey as PartialEq>::eq, key_equality)]
fn a_policy_that_admits_more_is_never_instant() {
    let state = State::any();
    let new = any_policy();
    let (amount, spend, usage): (u128, u128, u128) = (kani::any(), kani::any(), kani::any());
    let admits_more = decided(Some(&new), amount, spend, usage) == Decision::Allow
        && decided(state.policy.as_ref(), amount, spend, usage) != Decision::Allow;
    kani::cover!(admits_more);
    if admits_more {
        assert!(
            loosens(&policy_change(&new), &state),
            "a policy that admits a request the old one denied was classified as instant"
        );
    }
}

/// The converse: a replacement that is not loosening admits nothing the old
/// policy did not. (So a change that only tightens, or leaves equal, in every
/// field is exactly the instant case.)
#[kani::proof]
#[kani::unwind(12)]
#[kani::stub(<PublicKey as PartialEq>::eq, key_equality)]
fn a_policy_that_is_not_loosening_admits_nothing_new() {
    let state = State::any();
    let new = any_policy();
    let (amount, spend, usage): (u128, u128, u128) = (kani::any(), kani::any(), kani::any());
    if !loosens(&policy_change(&new), &state) {
        kani::cover!(decided(Some(&new), amount, spend, usage) == Decision::Allow);
        if decided(Some(&new), amount, spend, usage) == Decision::Allow {
            assert!(
                decided(state.policy.as_ref(), amount, spend, usage) == Decision::Allow,
                "a change classified as instant admits a request the old policy denied"
            );
        }
    }
}

// ---- Grant ----

/// Spec: a grant loosens when it extends a mandate that is live (exists, not
/// revoked, not yet expired) so that the agent is authorized at some time it was
/// not before, or when it asks for a fresh budget period while some of the
/// period's budget is spent (it hands that back). The authorized times of a
/// live mandate are those before its expiry; the witness `t` is any time from
/// now on. Any other grant (none, revoked, expired, no later expiry) is bounded
/// by the same policy for no longer than before, and is instant by design.
fn grant_case() -> (bool, bool) {
    let state = State::any();
    let expiry: u64 = kani::any();
    let reset_budget = any_option(|| kani::any::<bool>());
    let change = OwnerChange::GrantMandate {
        agent_key: key(1),
        expiry: U64(expiry),
        scope: None,
        reset_budget,
    };
    let actual = loosens(&change, &state);

    let live = state
        .mandate
        .filter(|mandate| !mandate.revoked && state.now < mandate.expiry);
    let newly_authorized = match live {
        Some(live) => {
            // Some time `t >= now` at which the new expiry authorizes the agent
            // and the live one does not.
            let t: u64 = kani::any();
            t >= state.now && t < expiry && !(t < live.expiry)
        }
        None => false,
    };
    let refunds_budget = reset_budget == Some(true) && state.budget_period_used;
    (actual, newly_authorized || refunds_budget)
}

#[kani::proof]
#[kani::unwind(12)]
fn extending_a_live_mandate_or_refunding_budget_is_never_instant() {
    let (actual, spec) = grant_case();
    kani::cover!(actual);
    kani::cover!(!actual);
    if spec {
        assert!(actual, "a loosening grant was classified as instant");
    }
}

/// The converse needs the existence of a witness, so it is stated on the
/// witness the code's own boundary names: the live expiry itself.
#[kani::proof]
#[kani::unwind(12)]
fn a_grant_is_loosening_only_if_it_extends_a_live_mandate_or_refunds_budget() {
    let state = State::any();
    let expiry: u64 = kani::any();
    let reset_budget = any_option(|| kani::any::<bool>());
    let change = OwnerChange::GrantMandate {
        agent_key: key(1),
        expiry: U64(expiry),
        scope: None,
        reset_budget,
    };
    if loosens(&change, &state) {
        let extends = match state.mandate {
            Some(live) => !live.revoked && state.now < live.expiry && expiry > live.expiry,
            None => false,
        };
        assert!(extends || (reset_budget == Some(true) && state.budget_period_used));
    }
}

// ---- Unfreeze ----

/// Spec: the contract is active after the change and was not before.
fn unfreeze_case() -> (bool, bool) {
    let state = State::any();
    let active_before = state.status == Status::Active;
    (loosens(&OwnerChange::Unfreeze, &state), !active_before)
}
classifier_proofs!(
    unfreezing_a_frozen_contract_is_never_instant,
    unfreeze_classification_is_exactly_the_spec,
    4,
    unfreeze_case
);

// ---- EVM configuration ----

fn evm_config_change(
    chain_id: u64,
    max_fee: u128,
    priority: u128,
    gas_limit: Option<u64>,
    token_gas_limit: Option<u64>,
) -> OwnerChange {
    OwnerChange::SetEvmConfig {
        signer_id: account_a(),
        derivation_path: "p".to_string(),
        sender: "0x0000000000000000000000000000000000000001".to_string(),
        chain_id,
        max_fee_per_gas: U128(max_fee),
        max_priority_fee_per_gas: U128(priority),
        gas_limit,
        token_gas_limit,
    }
}

/// Spec: a first configuration (it is what makes EVM payments possible), a
/// move to another chain, or any of the four bounds strictly raised: the
/// maximum fee, the maximum priority fee, the gas limit of a transfer, the gas
/// limit of a token transfer (an omitted gas limit is its default). Raising the
/// priority fee loosens by this spec although the contract accounts fees at
/// the maximum fee alone: a deliberate over-classification.
fn evm_config_case() -> (bool, bool) {
    let state = State::any();
    let chain_id: u64 = kani::any();
    let (max_fee, priority): (u128, u128) = (kani::any(), kani::any());
    let gas_limit = any_option(|| kani::any::<u64>());
    let token_gas_limit = any_option(|| kani::any::<u64>());
    let change = evm_config_change(chain_id, max_fee, priority, gas_limit, token_gas_limit);
    let spec = match state.evm {
        None => true,
        Some(old) => {
            let gas = gas_limit.unwrap_or(DEFAULT_GAS_LIMIT);
            let token_gas = token_gas_limit.unwrap_or(DEFAULT_TOKEN_GAS_LIMIT);
            chain_id != old.chain_id
                || max_fee > old.max_fee_per_gas
                || priority > old.max_priority_fee_per_gas
                || gas > old.gas_limit
                || token_gas > old.token_gas_limit
        }
    };
    (loosens(&change, &state), spec)
}
classifier_proofs!(
    raising_an_evm_bound_or_moving_the_chain_is_never_instant,
    evm_config_classification_is_exactly_the_spec,
    12,
    evm_config_case
);

// ---- Delay ----

/// Spec: the reaction window shrinks: the new delay is strictly shorter than
/// the current one (zero, which switches the timelock off, is the shortest).
fn delay_case() -> (bool, bool) {
    let state = State::any();
    let delay: u64 = kani::any();
    let change = OwnerChange::SetTimelockDelay { delay: U64(delay) };
    (loosens(&change, &state), delay < state.delay)
}
classifier_proofs!(
    a_shorter_delay_is_never_instant,
    delay_classification_is_exactly_the_spec,
    4,
    delay_case
);

// ---- Treasury ----

/// Spec: either recovery destination is not what it was. A new destination is
/// somewhere instant recovery may send funds. Clearing a treasury counts too: a
/// deliberate over-classification (it only makes recovery slower).
fn treasury_case() -> (bool, bool) {
    let state = State::any();
    let near = any_account();
    let evm_text = any_option(|| "0x0000000000000000000000000000000000000001".to_string());
    let change = OwnerChange::SetTreasury {
        near: near.clone(),
        evm: evm_text.clone(),
    };
    let proposed_evm = evm_text.map(|_| state.parsed_treasury);
    let spec = near != state.near_treasury || proposed_evm != state.evm_treasury;
    (loosens(&change, &state), spec)
}
classifier_proofs!(
    changing_a_treasury_is_never_instant,
    treasury_classification_is_exactly_the_spec,
    24,
    treasury_case
);

// ---- Guardian ----

/// Spec: the set of accounts that can veto (the guardian, at most one) loses a
/// member: the current guardian is not the guardian after the change. Adding
/// one where there was none loses nothing.
fn guardian_case() -> (bool, bool) {
    let state = State::any();
    let after = any_account();
    let change = OwnerChange::SetGuardian {
        guardian: after.clone(),
    };
    let loses_a_member = match &state.guardian {
        Some(current) => after.as_ref() != Some(current),
        None => false,
    };
    (loosens(&change, &state), loses_a_member)
}
classifier_proofs!(
    removing_or_replacing_the_guardian_is_never_instant,
    guardian_classification_is_exactly_the_spec,
    12,
    guardian_case
);

// ---- Owner recovery ----

/// Spec: instant only to the registered NEAR treasury; funds sent anywhere else
/// (including when there is no treasury) are a loosening.
fn near_withdraw_case() -> (bool, bool) {
    let state = State::any();
    let to = account_a();
    let change = OwnerChange::OwnerWithdraw {
        asset: AssetId::Near,
        amount: U128(kani::any()),
        to: to.clone(),
    };
    let to_the_treasury = state.near_treasury.as_ref() == Some(&to);
    (loosens(&change, &state), !to_the_treasury)
}
classifier_proofs!(
    paying_out_to_a_non_treasury_account_is_never_instant,
    near_withdrawal_classification_is_exactly_the_spec,
    12,
    near_withdraw_case
);

/// Spec: instant only to the registered EVM treasury or to this contract's own
/// derived address (a self-transfer moves nothing out). Any other destination
/// is a loosening, for a native and for a token withdrawal alike.
fn evm_withdraw_case<const TOKEN: bool>() -> (bool, bool) {
    let state = State::any();
    let recipient = "0x0000000000000000000000000000000000000001".to_string();
    let change = if TOKEN {
        OwnerChange::OwnerEvmTokenWithdraw {
            token: recipient.clone(),
            recipient,
            amount: U128(kani::any()),
            nonce: U64(kani::any()),
        }
    } else {
        OwnerChange::OwnerEvmWithdraw {
            recipient,
            value: U128(kani::any()),
            nonce: U64(kani::any()),
        }
    };
    let destination = state.parsed_address;
    let safe = state.evm_treasury == Some(destination) || state.evm_sender == Some(destination);
    (loosens(&change, &state), !safe)
}
fn evm_native_withdraw_case() -> (bool, bool) {
    evm_withdraw_case::<false>()
}
fn evm_token_withdraw_case() -> (bool, bool) {
    evm_withdraw_case::<true>()
}
classifier_proofs!(
    paying_out_to_an_unsafe_evm_address_is_never_instant,
    evm_withdrawal_classification_is_exactly_the_spec,
    24,
    evm_native_withdraw_case
);
classifier_proofs!(
    a_token_payout_to_an_unsafe_evm_address_is_never_instant,
    evm_token_withdrawal_classification_is_exactly_the_spec,
    24,
    evm_token_withdraw_case
);

/// `is_safe_destination` itself, over every 20-byte address.
#[kani::proof]
fn a_destination_is_safe_only_if_it_is_the_treasury_or_the_sender() {
    let destination: [u8; 20] = kani::any();
    let treasury = any_option(kani::any::<[u8; 20]>);
    let sender = any_option(kani::any::<[u8; 20]>);
    let safe = is_safe_destination(&destination, treasury.as_ref(), sender.as_ref());
    assert_eq!(
        safe,
        treasury == Some(destination) || sender == Some(destination)
    );
}
