//! The single policy decision function. `dry_run` and, later, the real action
//! path both call [`evaluate`] with state gathered by the contract, so the
//! rules live in exactly one place. It performs no I/O and reads no state.

use near_sdk::json_types::U128;
use near_sdk::{AccountId, PublicKey};

use crate::types::{AssetId, AssetPolicy, Decision, DenyReason, Headroom, Mandate, Status};

/// The recipient of a request, on whichever surface it is paid.
#[derive(Clone, Copy)]
pub enum RecipientRef<'a> {
    Near(&'a AccountId),
    Evm(&'a [u8; 20]),
    /// No recipient: the gas leg of a token payment, which pays the network
    /// and no one.
    None,
}

/// Which part of a payment a request is being evaluated for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Leg {
    /// The transfer itself: every check, including the recipient allowlist and
    /// the mandate's scope.
    Transfer,
    /// The gas of a token transfer, paid in the chain's native asset: the same
    /// authority and the native asset's global policy (cap, window, budget),
    /// but no recipient and no scope. A scope limits what the agent may *move*;
    /// gas is a cost, and the native policy is what bounds it.
    Gas,
}

pub struct EvalInput<'a> {
    pub status: Status,
    pub mandate: Option<&'a Mandate>,
    /// Current block timestamp in nanoseconds.
    pub now: u64,
    /// The key that signed the call, if the call reached this contract from its
    /// own account (the only way the agent's function-call key can reach it);
    /// `None` for a call from any other account.
    pub caller_key: Option<&'a PublicKey>,
    pub asset: &'a AssetId,
    pub policy: Option<&'a AssetPolicy>,
    pub recipient: RecipientRef<'a>,
    /// Whether the recipient is on the global allowlist of its surface.
    pub recipient_allowed: bool,
    pub amount: u128,
    /// Spend counted against the window limit: committed spend plus in-flight
    /// reservations. See [`effective_spend`].
    pub effective_spend: u128,
    /// Committed spend of the asset under the current mandate epoch plus what
    /// is in flight, counted against the policy's `epoch_budget` exactly as
    /// `effective_spend` is counted against the window. See [`effective_spend`].
    pub epoch_usage: u128,
}

/// What the window limit is checked against: spend already committed inside
/// the live window plus amounts reserved for transfers still in flight.
/// Saturates, so an impossible overflow fails closed (over any limit).
pub fn effective_spend(committed_live: u128, reserved: u128) -> u128 {
    committed_live.saturating_add(reserved)
}

/// The room a next request has under the policy's own constraints, given what
/// [`evaluate`] is given: `effective_spend` (see [`effective_spend`]) against the
/// window, and `epoch_usage` (the same function over the budget period's
/// committed spend) against the budget. It is the inverse of the checks
/// `evaluate` makes (`used + amount <= limit`): a request of `amount` passes the
/// cap, the window and the budget exactly when it is at most the smallest of the
/// three figures returned here, and when nothing is already over a limit (a
/// limit already exceeded leaves no room, not even for zero, and reads as 0).
/// The test `headroom_is_the_boundary_of_what_evaluate_allows` pins this against
/// `evaluate` itself.
///
/// Only the policy's constraints: a mandate's scope can narrow them further and
/// is not consulted.
pub fn headroom(policy: &AssetPolicy, effective_spend: u128, epoch_usage: u128) -> Headroom {
    Headroom {
        per_tx_cap: policy.per_tx_cap,
        window_remaining: U128(policy.window_limit.0.saturating_sub(effective_spend)),
        budget_remaining: policy
            .epoch_budget
            .map(|budget| U128(budget.0.saturating_sub(epoch_usage))),
    }
}

/// Checks run in a fixed order, from the broadest gate to the narrowest limit,
/// and the first failure decides the reason.
///
/// The global checks (status, mandate, key, the asset's policy, recipient
/// allowlist, caps and window) come first. Only a request that passes all of
/// them meets the mandate's scope, which is checked last (asset, recipient,
/// then its per-transaction cap). A scope therefore only ever subtracts: it
/// can turn an allow into a deny, never the reverse.
pub fn evaluate(input: &EvalInput) -> Decision {
    decision(deny_reason(input, Leg::Transfer))
}

/// The gas leg of a token payment: the checks of [`evaluate`] that do not
/// concern a recipient or the mandate's scope, for the native asset's policy.
pub fn evaluate_gas(input: &EvalInput) -> Decision {
    decision(deny_reason(input, Leg::Gas))
}

fn decision(reason: Option<DenyReason>) -> Decision {
    match reason {
        Some(reason) => Decision::Deny(reason),
        None => Decision::Allow,
    }
}

fn deny_reason(input: &EvalInput, leg: Leg) -> Option<DenyReason> {
    if input.status == Status::Frozen {
        return Some(DenyReason::Frozen);
    }

    let Some(mandate) = input.mandate else {
        return Some(DenyReason::NoMandate);
    };
    if mandate.revoked {
        return Some(DenyReason::MandateRevoked);
    }
    if input.now >= mandate.expiry.0 {
        return Some(DenyReason::MandateExpired);
    }
    if input.caller_key != Some(&mandate.agent_key) {
        return Some(DenyReason::NotAuthorized);
    }

    let Some(policy) = input.policy else {
        return Some(DenyReason::AssetNotAllowed);
    };
    if leg == Leg::Transfer && !input.recipient_allowed {
        return Some(DenyReason::RecipientNotAllowed);
    }
    if input.amount > policy.per_tx_cap.0 {
        return Some(DenyReason::PerTxCapExceeded);
    }
    // An overflowing total is over any limit.
    match input.effective_spend.checked_add(input.amount) {
        Some(total) if total <= policy.window_limit.0 => {}
        _ => return Some(DenyReason::WindowLimitExceeded),
    }
    // The lifetime budget of this grant. It is part of the asset's global
    // policy, so it is checked with the global checks, after the window: where
    // both would deny, the window is the reason, as it always was.
    if let Some(budget) = policy.epoch_budget {
        match input.epoch_usage.checked_add(input.amount) {
            Some(total) if total <= budget.0 => {}
            _ => return Some(DenyReason::MandateBudgetExceeded),
        }
    }
    match leg {
        Leg::Transfer => scope_denial(input, mandate),
        Leg::Gas => None,
    }
}

/// The mandate's scope, applied to a request that passed every global check
/// (including the budget).
/// `None` fields restrict nothing.
fn scope_denial(input: &EvalInput, mandate: &Mandate) -> Option<DenyReason> {
    let scope = &mandate.scope;
    if scope
        .allowed_assets
        .as_ref()
        .is_some_and(|assets| !assets.contains(input.asset))
    {
        return Some(DenyReason::AssetOutOfScope);
    }
    let recipient_out_of_scope = match input.recipient {
        RecipientRef::Near(recipient) => scope
            .allowed_near_recipients
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(recipient)),
        RecipientRef::Evm(address) => {
            let address = crate::evm::format_address(address);
            scope
                .allowed_evm_recipients
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(&address))
        }
        // A transfer always has a recipient.
        RecipientRef::None => false,
    };
    if recipient_out_of_scope {
        return Some(DenyReason::RecipientOutOfScope);
    }
    // The global cap already held, so this binds only when it is the tighter.
    if scope.per_tx_cap.is_some_and(|cap| input.amount > cap.0) {
        return Some(DenyReason::PerTxCapExceeded);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MandateScope;
    use near_sdk::json_types::{U128, U64};

    fn key(byte: u8) -> PublicKey {
        PublicKey::from_parts(near_sdk::CurveType::ED25519, vec![byte; 32]).unwrap()
    }

    fn mandate() -> Mandate {
        Mandate {
            agent_key: key(1),
            expiry: U64(1_000),
            epoch: 1,
            revoked: false,
            scope: MandateScope::default(),
        }
    }

    const POLICY: AssetPolicy = AssetPolicy {
        per_tx_cap: U128(10),
        window_limit: U128(25),
        epoch_budget: None,
    };

    fn run(adjust: impl FnOnce(&mut EvalInput)) -> Decision {
        run_as(Some(key(1)), adjust)
    }

    fn run_as(caller: Option<PublicKey>, adjust: impl FnOnce(&mut EvalInput)) -> Decision {
        run_with(mandate(), caller, adjust)
    }

    fn account(name: &str) -> AccountId {
        name.parse().unwrap()
    }

    /// A request for 5 of NEAR to `bob.near` by the mandate's key.
    fn run_with(
        mandate: Mandate,
        caller: Option<PublicKey>,
        adjust: impl FnOnce(&mut EvalInput),
    ) -> Decision {
        let bob = account("bob.near");
        decide(
            mandate,
            caller,
            &AssetId::Near,
            RecipientRef::Near(&bob),
            adjust,
        )
    }

    /// The same request for a given asset and recipient.
    fn decide(
        mandate: Mandate,
        caller: Option<PublicKey>,
        asset: &AssetId,
        recipient: RecipientRef,
        adjust: impl FnOnce(&mut EvalInput),
    ) -> Decision {
        let mut input = EvalInput {
            status: Status::Active,
            mandate: Some(&mandate),
            now: 500,
            caller_key: caller.as_ref(),
            asset,
            policy: Some(&POLICY),
            recipient,
            recipient_allowed: true,
            amount: 5,
            effective_spend: 0,
            epoch_usage: 0,
        };
        adjust(&mut input);
        evaluate(&input)
    }

    fn scoped(scope: MandateScope) -> Mandate {
        Mandate { scope, ..mandate() }
    }

    #[test]
    fn allows_a_compliant_request() {
        assert_eq!(run(|_| {}), Decision::Allow);
    }

    #[test]
    fn caps_are_inclusive() {
        assert_eq!(run(|i| i.amount = 10), Decision::Allow);
        assert_eq!(
            run(|i| i.amount = 11),
            Decision::Deny(DenyReason::PerTxCapExceeded)
        );
        assert_eq!(
            run(|i| {
                i.amount = 10;
                i.effective_spend = 15;
            }),
            Decision::Allow
        );
        assert_eq!(
            run(|i| {
                i.amount = 10;
                i.effective_spend = 16;
            }),
            Decision::Deny(DenyReason::WindowLimitExceeded)
        );
    }

    #[test]
    fn effective_spend_includes_reservations() {
        assert_eq!(effective_spend(10, 5), 15);
        assert_eq!(effective_spend(0, 7), 7);
        assert_eq!(effective_spend(u128::MAX, 1), u128::MAX);
        // A reservation alone can use up the window limit.
        assert_eq!(
            run(|i| {
                i.amount = 10;
                i.effective_spend = effective_spend(0, 16);
            }),
            Decision::Deny(DenyReason::WindowLimitExceeded)
        );
    }

    #[test]
    fn overflowing_total_is_denied() {
        assert_eq!(
            run(|i| i.effective_spend = u128::MAX),
            Decision::Deny(DenyReason::WindowLimitExceeded)
        );
    }

    #[test]
    fn only_the_mandated_key_is_authorized() {
        assert_eq!(
            run_as(Some(key(2)), |_| {}),
            Decision::Deny(DenyReason::NotAuthorized)
        );
        // A call that did not come from the contract's own account carries no key.
        assert_eq!(
            run_as(None, |_| {}),
            Decision::Deny(DenyReason::NotAuthorized)
        );
    }

    #[test]
    fn expiry_is_exclusive_of_the_expiry_instant() {
        assert_eq!(run(|i| i.now = 999), Decision::Allow);
        assert_eq!(
            run(|i| i.now = 1_000),
            Decision::Deny(DenyReason::MandateExpired)
        );
    }

    #[test]
    fn frozen_outranks_every_other_check() {
        assert_eq!(
            run(|i| {
                i.status = Status::Frozen;
                i.mandate = None;
                i.policy = None;
            }),
            Decision::Deny(DenyReason::Frozen)
        );
    }

    // ---- Mandate scope ----

    #[test]
    fn an_unscoped_mandate_is_the_global_policy_alone() {
        assert_eq!(
            run_with(scoped(MandateScope::default()), Some(key(1)), |_| {}),
            Decision::Allow
        );
        // Every global denial is untouched by the empty scope.
        let denied = |adjust: fn(&mut EvalInput)| run_with(mandate(), Some(key(1)), adjust);
        assert_eq!(
            denied(|i| i.amount = 11),
            Decision::Deny(DenyReason::PerTxCapExceeded)
        );
        assert_eq!(
            denied(|i| i.recipient_allowed = false),
            Decision::Deny(DenyReason::RecipientNotAllowed)
        );
    }

    #[test]
    fn the_scope_allows_what_it_names_and_denies_the_rest_of_what_the_policy_allows() {
        let scope = MandateScope {
            allowed_assets: Some(vec![AssetId::Near]),
            allowed_near_recipients: Some(vec![account("bob.near")]),
            ..MandateScope::default()
        };
        assert_eq!(
            run_with(scoped(scope.clone()), Some(key(1)), |_| {}),
            Decision::Allow
        );

        let (other_asset, bob, carol) = (
            AssetId::Nep141(account("usdc.near")),
            account("bob.near"),
            account("carol.near"),
        );
        assert_eq!(
            decide(
                scoped(scope.clone()),
                Some(key(1)),
                &other_asset,
                RecipientRef::Near(&bob),
                |_| {}
            ),
            Decision::Deny(DenyReason::AssetOutOfScope)
        );
        assert_eq!(
            decide(
                scoped(scope),
                Some(key(1)),
                &AssetId::Near,
                RecipientRef::Near(&carol),
                |_| {}
            ),
            Decision::Deny(DenyReason::RecipientOutOfScope)
        );
    }

    #[test]
    fn evm_recipients_are_scoped_apart_from_near_recipients() {
        let address = [0x11u8; 20];
        let other = [0x22u8; 20];
        let scope = MandateScope {
            allowed_evm_recipients: Some(vec![crate::evm::format_address(&address)]),
            ..MandateScope::default()
        };
        let evm = AssetId::Evm(5);
        let on_evm = |to: &[u8; 20]| {
            decide(
                scoped(scope.clone()),
                Some(key(1)),
                &evm,
                RecipientRef::Evm(to),
                |_| {},
            )
        };
        assert_eq!(on_evm(&address), Decision::Allow);
        assert_eq!(
            on_evm(&other),
            Decision::Deny(DenyReason::RecipientOutOfScope)
        );
        // The EVM list says nothing about NEAR recipients.
        assert_eq!(
            run_with(scoped(scope), Some(key(1)), |_| {}),
            Decision::Allow
        );
    }

    #[test]
    fn an_empty_list_permits_nothing_of_its_kind() {
        let no_assets = MandateScope {
            allowed_assets: Some(Vec::new()),
            ..MandateScope::default()
        };
        assert_eq!(
            run_with(scoped(no_assets), Some(key(1)), |_| {}),
            Decision::Deny(DenyReason::AssetOutOfScope)
        );
        let no_recipients = MandateScope {
            allowed_near_recipients: Some(Vec::new()),
            ..MandateScope::default()
        };
        assert_eq!(
            run_with(scoped(no_recipients), Some(key(1)), |_| {}),
            Decision::Deny(DenyReason::RecipientOutOfScope)
        );
    }

    #[test]
    fn the_effective_cap_is_the_smaller_of_the_global_and_the_mandates() {
        let capped = |cap: u128| MandateScope {
            per_tx_cap: Some(U128(cap)),
            ..MandateScope::default()
        };
        let amount =
            |mandate: Mandate, amount: u128| run_with(mandate, Some(key(1)), |i| i.amount = amount);
        // Tighter than global (10): binds.
        assert_eq!(amount(scoped(capped(4)), 4), Decision::Allow);
        assert_eq!(
            amount(scoped(capped(4)), 5),
            Decision::Deny(DenyReason::PerTxCapExceeded)
        );
        // Looser than global: does not widen, the global cap still binds.
        assert_eq!(amount(scoped(capped(1_000)), 10), Decision::Allow);
        assert_eq!(
            amount(scoped(capped(1_000)), 11),
            Decision::Deny(DenyReason::PerTxCapExceeded)
        );
    }

    #[test]
    fn the_global_checks_come_first_so_a_scope_never_widens() {
        // Everything is in scope, yet each global denial still decides.
        let all = MandateScope {
            allowed_assets: Some(vec![AssetId::Near]),
            allowed_near_recipients: Some(vec![account("bob.near")]),
            per_tx_cap: Some(U128(1_000)),
            ..MandateScope::default()
        };
        let denied =
            |adjust: fn(&mut EvalInput)| run_with(scoped(all.clone()), Some(key(1)), adjust);
        assert_eq!(
            denied(|i| i.policy = None),
            Decision::Deny(DenyReason::AssetNotAllowed)
        );
        assert_eq!(
            denied(|i| i.recipient_allowed = false),
            Decision::Deny(DenyReason::RecipientNotAllowed)
        );
        assert_eq!(
            denied(|i| i.amount = 11),
            Decision::Deny(DenyReason::PerTxCapExceeded)
        );
        assert_eq!(
            denied(|i| i.effective_spend = 21),
            Decision::Deny(DenyReason::WindowLimitExceeded)
        );
        assert_eq!(
            denied(|i| i.status = Status::Frozen),
            Decision::Deny(DenyReason::Frozen)
        );
        // And a request that is out of scope as well as over the window reports the window.
        let carol = account("carol.near");
        assert_eq!(
            decide(
                scoped(all),
                Some(key(1)),
                &AssetId::Near,
                RecipientRef::Near(&carol),
                |i| {
                    i.effective_spend = 21;
                }
            ),
            Decision::Deny(DenyReason::WindowLimitExceeded)
        );
    }

    // ---- Headroom ----

    #[test]
    fn headroom_is_the_boundary_of_what_evaluate_allows() {
        // For a grid of policies and usage, the largest amount `evaluate`
        // allows is the smallest figure of the headroom, one more is denied, and
        // where a limit is already exceeded nothing is allowed, not even zero.
        let budgets = [None, Some(0), Some(7), Some(40)];
        for cap in [0u128, 1, 10] {
            for window in [0u128, 5, 25] {
                for budget in budgets {
                    for spend in [0u128, 3, 25, 30] {
                        for usage in [0u128, 7, 39, 45] {
                            // Leaked so it is `'static` like the consts the other
                            // tests pass to `run`; a few hundred tiny values.
                            let policy: &'static AssetPolicy = Box::leak(Box::new(AssetPolicy {
                                per_tx_cap: U128(cap),
                                window_limit: U128(window),
                                epoch_budget: budget.map(U128),
                            }));
                            let room = headroom(policy, spend, usage);
                            let decide = |amount: u128| {
                                run(|i| {
                                    i.policy = Some(policy);
                                    i.effective_spend = spend;
                                    i.epoch_usage = usage;
                                    i.amount = amount;
                                })
                            };
                            let over = spend > window || budget.is_some_and(|b| usage > b);
                            let case = format!("{policy:?} spend {spend} usage {usage}");
                            if over {
                                assert_ne!(decide(0), Decision::Allow, "{case}");
                                continue;
                            }
                            let most = [Some(room.per_tx_cap.0), Some(room.window_remaining.0)]
                                .into_iter()
                                .chain([room.budget_remaining.map(|b| b.0)])
                                .flatten()
                                .min()
                                .unwrap();
                            assert_eq!(decide(most), Decision::Allow, "{case}");
                            assert_ne!(decide(most + 1), Decision::Allow, "{case}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn headroom_reads_zero_when_a_limit_is_used_up_or_over_and_has_no_budget_figure_without_a_budget(
    ) {
        let policy = AssetPolicy {
            per_tx_cap: U128(10),
            window_limit: U128(25),
            epoch_budget: None,
        };
        let room = headroom(&policy, 30, u128::MAX);
        assert_eq!(room.window_remaining, U128(0));
        assert_eq!(room.budget_remaining, None);
        assert_eq!(room.per_tx_cap, U128(10));
        let budgeted = headroom(&BUDGETED, 0, 45);
        assert_eq!(budgeted.budget_remaining, Some(U128(0)));
    }

    // ---- Lifetime budget ----

    const BUDGETED: AssetPolicy = AssetPolicy {
        epoch_budget: Some(U128(40)),
        ..POLICY
    };

    #[test]
    fn the_budget_counts_epoch_usage_plus_the_request_and_is_inclusive() {
        let with = |usage: u128, amount: u128| {
            run(|i| {
                i.policy = Some(&BUDGETED);
                i.epoch_usage = usage;
                i.amount = amount;
            })
        };
        assert_eq!(with(35, 5), Decision::Allow);
        assert_eq!(
            with(36, 5),
            Decision::Deny(DenyReason::MandateBudgetExceeded)
        );
        assert_eq!(
            with(u128::MAX, 1),
            Decision::Deny(DenyReason::MandateBudgetExceeded)
        );
    }

    #[test]
    fn no_budget_means_the_window_alone_however_much_was_spent() {
        assert_eq!(run(|i| i.epoch_usage = u128::MAX / 2), Decision::Allow);
    }

    #[test]
    fn the_window_is_the_reason_when_both_would_deny_and_the_budget_decides_otherwise() {
        let denied = |effective_spend: u128, epoch_usage: u128| {
            run(|i| {
                i.policy = Some(&BUDGETED);
                i.effective_spend = effective_spend;
                i.epoch_usage = epoch_usage;
            })
        };
        // Window 25, budget 40, request 5: only the window denies.
        assert_eq!(
            denied(21, 0),
            Decision::Deny(DenyReason::WindowLimitExceeded)
        );
        // Only the budget denies.
        assert_eq!(
            denied(0, 36),
            Decision::Deny(DenyReason::MandateBudgetExceeded)
        );
        // Both: the window, as before budgets existed.
        assert_eq!(
            denied(21, 36),
            Decision::Deny(DenyReason::WindowLimitExceeded)
        );
    }

    #[test]
    fn the_budget_is_a_global_check_so_it_comes_before_the_scope() {
        let scope = MandateScope {
            allowed_assets: Some(Vec::new()),
            ..MandateScope::default()
        };
        let denied = run_with(scoped(scope), Some(key(1)), |i| {
            i.policy = Some(&BUDGETED);
            i.epoch_usage = 40;
        });
        assert_eq!(denied, Decision::Deny(DenyReason::MandateBudgetExceeded));
    }
}
