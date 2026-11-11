//! The shadow model: an independent statement of what the contract *should*
//! do. It does not call `policy::evaluate` or `SpendRing`. Spend is a plain
//! list of `(time, amount)` and the window is arithmetic on slot numbers, so a
//! bug in the contract's ring or evaluation cannot be mirrored here.
//!
//! The accounting rules it encodes:
//! - A request is allowed only if every check passes, counted against
//!   `committed-in-window + reserved`.
//! - Allowing reserves the outlay; delivering the callback releases it exactly
//!   once.
//! - Local spend is booked unless the transfer failed.
//! - EVM spend is booked on **any successful signer receipt**, valid or merely
//!   unusable (G1), and only a failed receipt books nothing.

use std::collections::BTreeSet;

use crate::evm::Eip1559Tx;
use crate::types::DenyReason;

use super::actions::{Action, Asset, Outcome, PolicyVariant, Recipient, Tick};
use super::{NUM_BUCKETS, SLOT_LEN};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub cap: u128,
    pub limit: u128,
}

impl Policy {
    pub fn of(variant: PolicyVariant) -> Policy {
        let (cap, limit) = variant.limits();
        Policy { cap, limit }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelMandate {
    pub expiry: u64,
    pub epoch: u64,
    pub revoked: bool,
    /// Which of the two agent keys this mandate authenticates. Each grant
    /// switches to the other one, so a re-grant always changes the key (the
    /// contract refuses to re-grant the live key) and the previous key is the
    /// stale one.
    pub key: u8,
}

/// A dispatched request whose callback has not been delivered.
#[derive(Clone, Debug)]
pub struct Pending {
    pub asset: Asset,
    pub outlay: u128,
    /// The transaction the contract built, for EVM requests.
    pub tx: Option<Eip1559Tx>,
}

#[derive(Clone, Debug)]
pub struct Model {
    pub now: u64,
    pub frozen: bool,
    /// Boot grants a mandate and nothing removes it, so there is always one.
    pub mandate: ModelMandate,
    pub last_epoch: u64,
    pub policy: [Policy; 2],
    /// Committed spend per asset as `(booked at, amount)`.
    pub committed: [Vec<(u64, u128)>; 2],
    pub reserved: [u128; 2],
    /// The owner lowered this asset's limit below what was already counted;
    /// until it decays, `effective > limit` is expected for that asset.
    pub lowered: [bool; 2],
    pub next_nonce: u64,
    /// Every `(chain, nonce)` the contract has handed out, as observed in the
    /// transactions it built. History only: not part of the fingerprint.
    pub issued: BTreeSet<(u64, u64)>,
    pub queue: Vec<Pending>,
    /// Authority-loss ghost state for the monotonic-revocation invariant. It
    /// is maintained apart from `decide`, so the two are independent.
    ghost_frozen: bool,
    ghost_lapsed_epoch: Option<u64>,
}

impl Model {
    pub fn new(now: u64, mandate_expiry: u64) -> Model {
        Model {
            now,
            frozen: false,
            mandate: ModelMandate {
                expiry: mandate_expiry,
                epoch: 1,
                revoked: false,
                key: 0,
            },
            last_epoch: 1,
            policy: [Policy::of(PolicyVariant::Default); 2],
            committed: [Vec::new(), Vec::new()],
            reserved: [0; 2],
            lowered: [false; 2],
            next_nonce: 0,
            issued: BTreeSet::new(),
            queue: Vec::new(),
            ghost_frozen: false,
            ghost_lapsed_epoch: None,
        }
    }

    fn slot(time: u64) -> u64 {
        time / SLOT_LEN
    }

    /// Spend counts for the slot it was booked in and the `k` slots after it.
    fn is_live_at(now: u64, booked_at: u64) -> bool {
        Self::slot(now)
            .checked_sub(Self::slot(booked_at))
            .is_some_and(|age| age <= u64::from(NUM_BUCKETS))
    }

    pub fn live_committed(&self, asset: Asset) -> u128 {
        self.committed[asset.index()]
            .iter()
            .filter(|(booked_at, _)| Self::is_live_at(self.now, *booked_at))
            .fold(0u128, |sum, (_, amount)| sum.saturating_add(*amount))
    }

    pub fn effective(&self, asset: Asset) -> u128 {
        self.live_committed(asset)
            .saturating_add(self.reserved[asset.index()])
    }

    /// The expected decision for a request by the mandated agent, in the
    /// contract's documented order.
    ///
    /// `stale_key`: the request is signed with the key the mandate does not
    /// authenticate. Authentication ranks after the mandate checks.
    pub fn decide(
        &self,
        asset: Asset,
        outlay: u128,
        to: Recipient,
        stale_key: bool,
    ) -> Result<(), DenyReason> {
        if self.frozen {
            return Err(DenyReason::Frozen);
        }
        if self.mandate.revoked {
            return Err(DenyReason::MandateRevoked);
        }
        if self.now >= self.mandate.expiry {
            return Err(DenyReason::MandateExpired);
        }
        if stale_key {
            return Err(DenyReason::NotAuthorized);
        }
        let policy = self.policy[asset.index()];
        if to == Recipient::Disallowed {
            return Err(DenyReason::RecipientNotAllowed);
        }
        if outlay > policy.cap {
            return Err(DenyReason::PerTxCapExceeded);
        }
        match self.effective(asset).checked_add(outlay) {
            Some(total) if total <= policy.limit => Ok(()),
            _ => Err(DenyReason::WindowLimitExceeded),
        }
    }

    /// After time or state changed: forget spend that can never count again
    /// (time only moves forward), note an expiry, and re-arm the limit check.
    pub fn settle_bookkeeping(&mut self) {
        let now = self.now;
        for asset in super::actions::ASSETS {
            let i = asset.index();
            let live: Vec<(u64, u128)> = self.committed[i]
                .iter()
                .copied()
                .filter(|(booked_at, _)| Self::is_live_at(now, *booked_at))
                .collect();
            self.committed[i] = live;
            if self.effective(asset) <= self.policy[i].limit {
                self.lowered[i] = false;
            }
        }
        if now >= self.mandate.expiry {
            self.ghost_lapsed_epoch = Some(self.mandate.epoch);
        }
    }

    /// The agent must be refused, by the independent ghost bookkeeping:
    /// frozen until unfrozen, and a revoked or expired mandate stays lapsed
    /// until a new grant.
    pub fn authority_is_lost(&self) -> bool {
        self.ghost_frozen || self.ghost_lapsed_epoch == Some(self.mandate.epoch)
    }

    pub fn set_frozen(&mut self, frozen: bool) {
        self.frozen = frozen;
        self.ghost_frozen = frozen;
    }

    /// The key the mandate does not authenticate.
    pub fn stale_key(&self) -> u8 {
        1 - self.mandate.key
    }

    pub fn grant(&mut self, expiry: u64) {
        self.last_epoch += 1;
        self.mandate = ModelMandate {
            expiry,
            epoch: self.last_epoch,
            revoked: false,
            key: self.stale_key(),
        };
    }

    pub fn revoke(&mut self) {
        self.mandate.revoked = true;
        self.ghost_lapsed_epoch = Some(self.mandate.epoch);
    }

    pub fn set_policy(&mut self, asset: Asset, variant: PolicyVariant) {
        let policy = Policy::of(variant);
        self.policy[asset.index()] = policy;
        self.lowered[asset.index()] = self.effective(asset) > policy.limit;
    }

    pub fn reserve(&mut self, asset: Asset, outlay: u128) {
        let i = asset.index();
        self.reserved[i] = self.reserved[i].saturating_add(outlay);
    }

    /// Delivers one in-flight callback: release exactly once, and book unless
    /// the receipt failed.
    pub fn deliver(&mut self, index: usize, outcome: Outcome) -> Pending {
        let pending = self.queue.remove(index);
        let i = pending.asset.index();
        self.reserved[i] = self.reserved[i].saturating_sub(pending.outlay);
        if outcome.books_spend() {
            self.committed[i].push((self.now, pending.outlay));
        }
        pending
    }

    pub fn advance(&mut self, tick: Tick) {
        self.now += tick.nanos();
    }

    /// Whether `action` would leave the whole world unchanged. The exhaustive
    /// search skips these: they cannot reach a new state.
    pub fn is_noop(&self, action: Action) -> bool {
        match action {
            Action::Deliver { .. } => self.queue.is_empty(),
            Action::Freeze => self.frozen,
            Action::Unfreeze => !self.frozen,
            Action::SetPolicy { asset, variant } => {
                self.policy[asset.index()] == Policy::of(variant)
            }
            Action::RevokeMandate => self.mandate.revoked,
            _ => false,
        }
    }

    /// Everything that can influence future behaviour, for state dedup.
    pub fn canonical(&self) -> String {
        format!(
            "{:?}",
            (
                self.now,
                self.frozen,
                self.mandate,
                self.last_epoch,
                self.policy,
                &self.committed,
                self.reserved,
                self.lowered,
                self.next_nonce,
                &self.queue,
                self.ghost_frozen,
                self.ghost_lapsed_epoch,
            )
        )
    }
}
