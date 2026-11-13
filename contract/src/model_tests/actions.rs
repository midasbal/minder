//! The harness's action and result alphabets. Small and closed so that
//! sequences can be enumerated exhaustively.

use super::FEE_COST;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asset {
    Near,
    /// Native ETH.
    Evm,
    /// An ERC-20 token on the same chain. A token payment has two legs: its
    /// amount against this asset and its gas against `Evm`.
    Token,
}

pub const ASSETS: [Asset; ASSET_COUNT] = [Asset::Near, Asset::Evm, Asset::Token];
pub const ASSET_COUNT: usize = 3;

impl Asset {
    pub fn index(self) -> usize {
        match self {
            Asset::Near => 0,
            Asset::Evm => 1,
            Asset::Token => 2,
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
    /// The default cap and window, plus a lifetime budget of 70 per mandate
    /// grant: it binds before the window of 100 does.
    Budget70,
    /// The default cap and window, plus a lifetime budget of 150: the window
    /// binds first within a window, the budget once spend has accumulated across
    /// windows.
    Budget150,
}

impl PolicyVariant {
    /// `(per_tx_cap, window_limit)`.
    pub fn limits(self) -> (u128, u128) {
        match self {
            PolicyVariant::Tight => (30, 60),
            PolicyVariant::Default => (50, 100),
            PolicyVariant::Loose => (80, 160),
            PolicyVariant::Budget70 | PolicyVariant::Budget150 => (50, 100),
        }
    }

    /// The per-grant lifetime budget, if any.
    pub fn budget(self) -> Option<u128> {
        match self {
            PolicyVariant::Budget70 => Some(70),
            PolicyVariant::Budget150 => Some(150),
            _ => None,
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
/// - `OtherSigner`: the receipt succeeded with a well-formed signature over
///   the right hash by a key that is not the configured sender; booked, but
///   never emitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Fail,
    WrongShape,
    Garbage,
    Oversized,
    OtherSigner,
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

/// What a mandate is granted beyond the global policy, chosen so that each
/// kind of restriction both binds and, where it should, does not. Its meaning
/// is restated independently in the shadow model.
///
/// The harness has one allowed and one disallowed recipient per surface and
/// per-transaction caps of 30, 50 and 80 across the policy variants, so:
/// - `NearOnly`: only the NEAR asset (every EVM request is out of scope);
/// - `AllowedRecipientsOnly`: exactly the globally allowed recipients (a scope
///   that names everything the policy allows, so it restricts nothing);
/// - `DisallowedRecipientsOnly`: only the globally disallowed recipients (an
///   allowed recipient is out of scope, and a disallowed one is still refused
///   by the global allowlist);
/// - `CapTight`: a cap of 40, below the default global cap of 50;
/// - `CapLoose`: a cap of 100, above every global cap, so it never binds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Unscoped,
    NearOnly,
    AllowedRecipientsOnly,
    DisallowedRecipientsOnly,
    CapTight,
    CapLoose,
}

pub const SCOPES: [Scope; 6] = [
    Scope::Unscoped,
    Scope::NearOnly,
    Scope::AllowedRecipientsOnly,
    Scope::DisallowedRecipientsOnly,
    Scope::CapTight,
    Scope::CapLoose,
];

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

/// Who the guardian is, or is set to be: nobody, or one of two accounts (so a
/// replacement is a real change, and a replaced guardian is a real stranger).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guardian {
    Nobody,
    A,
    B,
}

pub const GUARDIANS: [Guardian; 3] = [Guardian::Nobody, Guardian::A, Guardian::B];

impl Guardian {
    /// The account slot (0 or 1), or none.
    pub fn slot(self) -> Option<u8> {
        match self {
            Guardian::Nobody => None,
            Guardian::A => Some(0),
            Guardian::B => Some(1),
        }
    }
}

/// An owner method the guardian must never be able to use. Each is tried with
/// arguments that would be valid for the owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forbidden {
    LoosenPolicy,
    Grant,
    Unfreeze,
    Withdraw,
    SetTreasury,
    Propose,
    Execute,
    SetGuardian,
}

pub const FORBIDDEN: [Forbidden; 8] = [
    Forbidden::LoosenPolicy,
    Forbidden::Grant,
    Forbidden::Unfreeze,
    Forbidden::Withdraw,
    Forbidden::SetTreasury,
    Forbidden::Propose,
    Forbidden::Execute,
    Forbidden::SetGuardian,
];

/// What an account holding a guardian's key tries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardianCall {
    Freeze,
    Revoke,
    Cancel(Pick),
    Forbidden(Forbidden),
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
    GrantMandate(Grant, Scope),
    /// A request signed with the key of the mandate granted before the current
    /// one (or, before any rotation, a key never granted): the agent's old key
    /// after a re-grant. It must be refused as `NotAuthorized` once the mandate
    /// checks that rank above it pass, and never approved.
    StaleKeyRequest(Asset),
    RevokeMandate,
    Freeze,
    Unfreeze,
    Advance(Tick),
    /// Propose a policy change through the timelock. Accepted only for a
    /// change that loosens, while the timelock is on.
    ProposePolicy {
        asset: Asset,
        variant: PolicyVariant,
    },
    /// Propose lifting a freeze through the timelock.
    ProposeUnfreeze,
    /// Execute the oldest or newest pending change, if it is due.
    ExecutePending(Pick),
    /// Cancel the oldest or newest pending change.
    CancelPending(Pick),
    /// The owner sets the guardian directly; refused if it removes or replaces
    /// one while the timelock is on.
    SetGuardian(Guardian),
    /// The owner proposes a guardian change through the timelock.
    ProposeGuardian(Guardian),
    /// Account `who` (0 or 1) makes a call a guardian might make. Only the
    /// current guardian's three stop-powers may succeed.
    Guardian {
        who: u8,
        call: GuardianCall,
    },
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

const OUTCOMES: [Outcome; 6] = [
    Outcome::Ok,
    Outcome::Fail,
    Outcome::WrongShape,
    Outcome::Garbage,
    Outcome::Oversized,
    Outcome::OtherSigner,
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
            PolicyVariant::Budget70,
            PolicyVariant::Budget150,
        ] {
            actions.push(Action::SetPolicy { asset, variant });
        }
    }
    for pick in [Pick::Oldest, Pick::Newest] {
        for outcome in OUTCOMES {
            actions.push(deliver(pick, outcome));
        }
    }
    for asset in ASSETS {
        actions.push(Action::StaleKeyRequest(asset));
    }
    for asset in ASSETS {
        for variant in [
            PolicyVariant::Tight,
            PolicyVariant::Default,
            PolicyVariant::Loose,
            PolicyVariant::Budget70,
            PolicyVariant::Budget150,
        ] {
            actions.push(Action::ProposePolicy { asset, variant });
        }
    }
    actions.push(Action::ProposeUnfreeze);
    for pick in [Pick::Oldest, Pick::Newest] {
        actions.push(Action::ExecutePending(pick));
        actions.push(Action::CancelPending(pick));
    }
    for scope in SCOPES {
        for grant in [Grant::Short, Grant::Long] {
            actions.push(Action::GrantMandate(grant, scope));
        }
    }
    actions.extend(guardian_alphabet());
    actions.extend([
        Action::RevokeMandate,
        Action::Freeze,
        Action::Unfreeze,
        Action::Advance(Tick::Sub),
        Action::Advance(Tick::Slot),
        Action::Advance(Tick::Window),
    ]);
    actions
}

/// Every guardian action: the owner's governance of the role, and what each of
/// the two guardian accounts may try.
pub fn guardian_alphabet() -> Vec<Action> {
    let mut actions = Vec::new();
    for guardian in GUARDIANS {
        actions.push(Action::SetGuardian(guardian));
        actions.push(Action::ProposeGuardian(guardian));
    }
    for who in [0, 1] {
        let mut calls = vec![
            GuardianCall::Freeze,
            GuardianCall::Revoke,
            GuardianCall::Cancel(Pick::Oldest),
            GuardianCall::Cancel(Pick::Newest),
        ];
        calls.extend(FORBIDDEN.map(GuardianCall::Forbidden));
        for call in calls {
            actions.push(Action::Guardian { who, call });
        }
    }
    actions
}
