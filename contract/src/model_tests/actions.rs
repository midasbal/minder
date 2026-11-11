//! The harness's action and result alphabets. Small and closed so that
//! sequences can be enumerated exhaustively.

use super::FEE_COST;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asset {
    Near,
    Evm,
}

pub const ASSETS: [Asset; 2] = [Asset::Near, Asset::Evm];

impl Asset {
    pub fn index(self) -> usize {
        match self {
            Asset::Near => 0,
            Asset::Evm => 1,
        }
    }
}

/// How much one request costs the policy, chosen around the default per-tx
/// cap (50) and window limit (100). For EVM this is the *outlay*, so the value
/// sent is `outlay - FEE_COST`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Amount {
    Mid,
    AtCap,
    OverCap,
}

impl Amount {
    pub fn outlay(self) -> u128 {
        match self {
            Amount::Mid => 30,
            Amount::AtCap => 50,
            Amount::OverCap => 51,
        }
    }

    /// The EVM value that produces this outlay.
    pub fn evm_value(self) -> u128 {
        self.outlay() - FEE_COST
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recipient {
    Allowed,
    Disallowed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyVariant {
    Tight,
    Default,
    Loose,
}

impl PolicyVariant {
    /// `(per_tx_cap, window_limit)`.
    pub fn limits(self) -> (u128, u128) {
        match self {
            PolicyVariant::Tight => (30, 60),
            PolicyVariant::Default => (50, 100),
            PolicyVariant::Loose => (80, 160),
        }
    }
}

/// Which in-flight callback to deliver next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Oldest,
    Newest,
}

/// What the dispatched call's promise resolves to.
///
/// - `Ok`: the transfer succeeded / the signer returned a valid signature.
/// - `Fail`: the receipt failed (a timeout, for the signer).
/// - `WrongShape`, `Garbage`, `Oversized`: the receipt *succeeded* but its
///   output is not a usable signature. For a local transfer these are a
///   success carrying an unexpected payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Fail,
    WrongShape,
    Garbage,
    Oversized,
}

impl Outcome {
    /// Only a failed receipt books nothing (G1 and the local rule).
    pub fn books_spend(self) -> bool {
        self != Outcome::Fail
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grant {
    Short,
    Long,
}

impl Grant {
    pub fn lifetime(self) -> u64 {
        match self {
            Grant::Short => 150,
            Grant::Long => super::LONG_MANDATE,
        }
    }
}

/// Time advances sized around the 100 ns slots and 400 ns window: inside a
/// slot, across one slot boundary, and past a whole window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    Sub,
    Slot,
    Window,
}

impl Tick {
    pub fn nanos(self) -> u64 {
        match self {
            Tick::Sub => 60,
            Tick::Slot => 100,
            Tick::Window => 500,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// The agent asks to pay. Two-phase: an allowed request goes on the
    /// in-flight queue.
    Request {
        asset: Asset,
        amount: Amount,
        to: Recipient,
    },
    /// Deliver the callback of an in-flight request.
    Deliver {
        pick: Pick,
        outcome: Outcome,
    },
    SetPolicy {
        asset: Asset,
        variant: PolicyVariant,
    },
    GrantMandate(Grant),
    RevokeMandate,
    Freeze,
    Unfreeze,
    Advance(Tick),
}

pub const fn request(asset: Asset, amount: Amount) -> Action {
    Action::Request {
        asset,
        amount,
        to: Recipient::Allowed,
    }
}

pub const fn deliver(pick: Pick, outcome: Outcome) -> Action {
    Action::Deliver { pick, outcome }
}

const OUTCOMES: [Outcome; 5] = [
    Outcome::Ok,
    Outcome::Fail,
    Outcome::WrongShape,
    Outcome::Garbage,
    Outcome::Oversized,
];

/// Every action: the alphabet of the random driver.
pub fn full_alphabet() -> Vec<Action> {
    let mut actions = Vec::new();
    for asset in ASSETS {
        for amount in [Amount::Mid, Amount::AtCap, Amount::OverCap] {
            actions.push(request(asset, amount));
        }
        actions.push(Action::Request {
            asset,
            amount: Amount::Mid,
            to: Recipient::Disallowed,
        });
        for variant in [
            PolicyVariant::Tight,
            PolicyVariant::Default,
            PolicyVariant::Loose,
        ] {
            actions.push(Action::SetPolicy { asset, variant });
        }
    }
    for pick in [Pick::Oldest, Pick::Newest] {
        for outcome in OUTCOMES {
            actions.push(deliver(pick, outcome));
        }
    }
    actions.extend([
        Action::GrantMandate(Grant::Short),
        Action::GrantMandate(Grant::Long),
        Action::RevokeMandate,
        Action::Freeze,
        Action::Unfreeze,
        Action::Advance(Tick::Sub),
        Action::Advance(Tick::Slot),
        Action::Advance(Tick::Window),
    ]);
    actions
}
