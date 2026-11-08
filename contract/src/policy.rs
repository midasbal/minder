//! The single policy decision function. `dry_run` and, later, the real action
//! path both call [`evaluate`] with state gathered by the contract, so the
//! rules live in exactly one place. It performs no I/O and reads no state.

use near_sdk::AccountId;

use crate::types::{AssetPolicy, Decision, DenyReason, Mandate, Status};

pub struct EvalInput<'a> {
    pub status: Status,
    pub mandate: Option<&'a Mandate>,
    /// Current block timestamp in nanoseconds.
    pub now: u64,
    /// The account attempting the action.
    pub caller: &'a AccountId,
    pub policy: Option<&'a AssetPolicy>,
    pub recipient_allowed: bool,
    pub amount: u128,
    /// Spend counted against the window limit: committed spend plus in-flight
    /// reservations. See [`effective_spend`].
    pub effective_spend: u128,
}

/// What the window limit is checked against: spend already committed inside
/// the live window plus amounts reserved for transfers still in flight.
/// Saturates, so an impossible overflow fails closed (over any limit).
pub fn effective_spend(committed_live: u128, reserved: u128) -> u128 {
    committed_live.saturating_add(reserved)
}

/// Checks run in a fixed order, from the broadest gate to the narrowest limit,
/// and the first failure decides the reason.
pub fn evaluate(input: &EvalInput) -> Decision {
    match deny_reason(input) {
        Some(reason) => Decision::Deny(reason),
        None => Decision::Allow,
    }
}

fn deny_reason(input: &EvalInput) -> Option<DenyReason> {
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
    if input.caller != &mandate.agent {
        return Some(DenyReason::NotAuthorized);
    }

    let Some(policy) = input.policy else {
        return Some(DenyReason::AssetNotAllowed);
    };
    if !input.recipient_allowed {
        return Some(DenyReason::RecipientNotAllowed);
    }
    if input.amount > policy.per_tx_cap.0 {
        return Some(DenyReason::PerTxCapExceeded);
    }
    // An overflowing total is over any limit.
    match input.effective_spend.checked_add(input.amount) {
        Some(total) if total <= policy.window_limit.0 => None,
        _ => Some(DenyReason::WindowLimitExceeded),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use near_sdk::json_types::{U128, U64};

    fn account(name: &str) -> AccountId {
        name.parse().unwrap()
    }

    fn mandate() -> Mandate {
        Mandate {
            agent: account("agent.near"),
            expiry: U64(1_000),
            epoch: 1,
            revoked: false,
        }
    }

    const POLICY: AssetPolicy = AssetPolicy {
        per_tx_cap: U128(10),
        window_limit: U128(25),
    };

    fn run(adjust: impl FnOnce(&mut EvalInput)) -> Decision {
        let mandate = mandate();
        let agent = account("agent.near");
        let mut input = EvalInput {
            status: Status::Active,
            mandate: Some(&mandate),
            now: 500,
            caller: &agent,
            policy: Some(&POLICY),
            recipient_allowed: true,
            amount: 5,
            effective_spend: 0,
        };
        adjust(&mut input);
        evaluate(&input)
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
}
