//! A `World` is the real contract's persisted state (the mocked storage,
//! including the borsh contract state) paired with the shadow model.
//! `World::apply` restores that storage, runs one action against the real
//! contract with the right caller, time and promise result, checks every
//! invariant against the model, and returns the next world. Being a plain
//! value, a world can be cloned, which is what lets the exhaustive search share
//! prefixes and deduplicate states.

use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::panic::{self, AssertUnwindSafe};
use std::sync::Once;

use near_sdk::borsh::BorshDeserialize;
use near_sdk::json_types::{U128, U64};
use near_sdk::mock::MockAction;
use near_sdk::serde_json::{json, Value};
use near_sdk::test_utils::{get_created_receipts, get_logs, VMContextBuilder};
use near_sdk::{
    env, Gas, MockedBlockchain, PromiseResult, PublicKey, RuntimeFeesConfig, VMContext,
};

use crate::evm::{self, Eip1559Tx};
use crate::test_support::{
    account, decision_events, fixture_signature, function_call, signer_response, test_sender_hex,
    Signer,
};
use crate::types::{Decision, Status};
use crate::{AssetId, MandateScope, Minder, OwnerChange};

use super::actions::{
    Action, Amount, Asset, Forbidden, GuardianCall, Outcome, Pick, Recipient, Scope, ASSETS,
    ASSET_COUNT,
};
use super::loss_bound::Booking;
use super::model::{Held, Model, Pending};
use super::{
    CHAIN_ID, DERIVATION_PATH, EVM_ALLOWED, EVM_DISALLOWED, GAS_LIMIT, LONG_MANDATE, MAX_FEE,
    MAX_PRIORITY_FEE, NEAR_ALLOWED, NEAR_DISALLOWED, NUM_BUCKETS, T0, TIMELOCK_DELAY,
    TOKEN_ADDRESS, TOKEN_GAS, TOKEN_GAS_LIMIT, WINDOW_LEN,
};

const OWNER: &str = "owner.near";
const CONTRACT: &str = "minder.near";
const SIGNER: &str = "signer.near";

/// Well-formed numbers for the response-shape cases, which never reach a
/// signature check. Real signatures come from the fixture.
const R: &str = "5b2fc0e1f6d4a7c98e3b1d2a4f5c6e7d8a9b0c1d2e3f405162738495a6b7c8d9";
const S: &str = "2a4d6f8091b3d5f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f";

type Storage = HashMap<Vec<u8>, Vec<u8>>;

/// An invariant failed, or the contract misbehaved.
#[derive(Debug)]
pub struct Violation(pub String);

macro_rules! ensure {
    ($condition:expr, $($message:tt)+) => {
        if !$condition {
            return Err(Violation(format!($($message)+)));
        }
    };
}

/// What a step did, for coverage accounting.
#[derive(Clone, Copy, Debug)]
pub enum Observed {
    Allowed(Asset),
    /// Denied on the transfer leg (or the only leg).
    Denied(Asset, crate::DenyReason),
    /// A token payment whose transfer leg passed and whose gas leg was denied.
    GasDenied(crate::DenyReason),
    Delivered(Asset, Outcome),
    /// A guardian call: what it was and whether it was allowed or refused.
    Guardian(&'static str),
    Other,
}

impl Observed {
    /// A coverage key: what happened, ignoring amounts.
    pub fn key(self) -> String {
        match self {
            Observed::Allowed(asset) => format!("Allowed({asset:?})"),
            Observed::Denied(asset, reason) => format!("Denied({asset:?}, {reason:?})"),
            Observed::GasDenied(reason) => format!("GasDenied({reason:?})"),
            Observed::Delivered(asset, outcome) => format!("Delivered({asset:?}, {outcome:?})"),
            Observed::Guardian(what) => format!("Guardian({what})"),
            Observed::Other => "Other".to_string(),
        }
    }
}

#[derive(Clone)]
pub struct World {
    storage: Storage,
    pub model: Model,
}

impl World {
    /// Initialized, armed for every asset, with a long mandate, and with the
    /// timelock off: every owner change is instant.
    pub fn boot() -> World {
        Self::boot_with_delay(0)
    }

    /// As `boot`, with the timelock switched on after the contract is armed
    /// (arming adds assets and recipients, which a timelock would hold back;
    /// raising the delay is itself instant).
    pub fn boot_timelocked() -> World {
        Self::boot_with_delay(TIMELOCK_DELAY)
    }

    fn boot_with_delay(delay: u64) -> World {
        set_env(context(OWNER, T0, None), Vec::new(), HashMap::new());
        let mut minder = Minder::init(
            account(OWNER),
            U64(WINDOW_LEN),
            NUM_BUCKETS,
            None,
            None,
            None,
            None,
        );
        for asset in ASSETS {
            let policy = super::model::Policy::of(super::actions::PolicyVariant::Default);
            minder.set_asset_policy(
                asset_id(asset),
                U128(policy.cap),
                U128(policy.limit),
                policy.budget.map(U128),
            );
        }
        minder.set_recipient(account(NEAR_ALLOWED), true);
        minder.set_evm_recipient(EVM_ALLOWED.to_string(), true);
        minder.set_evm_config(
            account(SIGNER),
            DERIVATION_PATH.to_string(),
            test_sender_hex(),
            CHAIN_ID,
            U128(MAX_FEE),
            U128(MAX_PRIORITY_FEE),
            Some(GAS_LIMIT),
            Some(TOKEN_GAS_LIMIT),
        );
        minder.grant_mandate(agent_key(0), U64(T0 + LONG_MANDATE), None);
        // Adding a guardian is instant, so it is in place before the timelock.
        minder.set_guardian(Some(guardian_account(0)));
        let mut model = Model::new(T0, T0 + LONG_MANDATE);
        model.guardian = Some(0);
        if delay > 0 {
            minder.set_timelock_delay(U64(delay));
            model.delay = delay;
        }
        World {
            storage: persist(minder),
            model,
        }
    }

    /// Identifies the world for state deduplication: the full stored state
    /// (so hidden ring layout counts) and everything the model carries
    /// forward.
    pub fn fingerprint(&self) -> u64 {
        let mut entries: Vec<(&Vec<u8>, &Vec<u8>)> = self.storage.iter().collect();
        entries.sort();
        let mut hasher = DefaultHasher::new();
        entries.hash(&mut hasher);
        self.model.canonical().hash(&mut hasher);
        hasher.finish()
    }

    pub fn apply(&self, action: Action) -> Result<(World, Observed), Violation> {
        let mut model = self.model.clone();
        if let Action::Advance(tick) = action {
            model.advance(tick);
            model.settle_bookkeeping();
        }

        // A delivery needs something in flight; with nothing it does nothing.
        let delivery = match action {
            Action::Deliver { pick, outcome } if !model.queue.is_empty() => {
                let index = match pick {
                    Pick::Oldest => 0,
                    Pick::Newest => model.queue.len() - 1,
                };
                Some((index, outcome))
            }
            _ => None,
        };
        // What this step's delivery should do to the window total.
        let mut delivered = [0u128; ASSET_COUNT];
        if let Some((index, outcome)) = delivery {
            if outcome.books_spend() {
                for (asset, amount) in model.queue[index].legs() {
                    delivered[asset.index()] += amount;
                }
            }
        }
        // The agent calls through its function-call key on the contract's own
        // account: the contract is the predecessor and the key is what tells
        // the agent from anyone else.
        let (caller, signer_key, promise_results) = match (action, delivery) {
            (Action::Request { .. } | Action::Advance(_), _) | (Action::Deliver { .. }, None) => {
                (CONTRACT, Some(agent_key(model.mandate.key)), Vec::new())
            }
            (Action::StaleKeyRequest(_), _) => {
                (CONTRACT, Some(agent_key(model.stale_key())), Vec::new())
            }
            (Action::Deliver { .. }, Some((index, outcome))) => (
                CONTRACT,
                None,
                vec![promise_result(&model.queue[index], outcome)],
            ),
            (Action::Guardian { who, .. }, _) => {
                (GUARDIAN_ACCOUNTS[usize::from(who)], None, Vec::new())
            }
            _ => (OWNER, None, Vec::new()),
        };

        set_env(
            context(caller, model.now, signer_key),
            promise_results,
            self.storage.clone(),
        );
        let mut minder: Minder = env::state_read()
            .ok_or_else(|| Violation("the contract state was not persisted".to_string()))?;
        let live_before = ASSETS.map(|asset| live_spend(&minder, asset));

        let observed = match action {
            Action::Request { asset, amount, to } => request(
                &mut minder,
                &mut model,
                asset,
                amount.outlay(),
                amount.evm_value(),
                to,
                false,
            )?,
            // A mid-sized request to an allowed recipient, so that only the
            // key can be the reason for a denial.
            Action::StaleKeyRequest(asset) => request(
                &mut minder,
                &mut model,
                asset,
                Amount::Mid.outlay(),
                Amount::Mid.evm_value(),
                Recipient::Allowed,
                true,
            )?,
            Action::Deliver { .. } => match delivery {
                Some((index, outcome)) => deliver(&mut minder, &mut model, index, outcome)?,
                None => Observed::Other,
            },
            // A policy change takes effect at once if it only tightens (or is
            // equal), and is refused if it loosens while the timelock is on. The
            // model decides which, from the rule alone.
            Action::SetPolicy { asset, variant } => {
                let (cap, limit) = variant.limits();
                let budget = variant.budget().map(U128);
                let attempt = guarded(|| {
                    minder.set_asset_policy(asset_id(asset), U128(cap), U128(limit), budget)
                });
                if model.policy_timelocked(asset, variant) {
                    expect_timelocked(attempt, "set_asset_policy")?;
                } else {
                    expect_applied(attempt, "set_asset_policy")?;
                    model.set_policy(asset, variant);
                }
                Observed::Other
            }
            Action::GrantMandate(grant, scope) => {
                let expiry = model.now + grant.lifetime();
                // Every grant switches to the other key, as the contract requires
                // a new key while a mandate is live.
                let key = agent_key(model.stale_key());
                let attempt =
                    guarded(|| minder.grant_mandate(key, U64(expiry), grant_scope(scope)));
                if model.grant_timelocked(expiry) {
                    expect_timelocked(attempt, "grant_mandate")?;
                } else {
                    expect_applied(attempt, "grant_mandate")?;
                    model.grant(expiry, scope);
                }
                Observed::Other
            }
            Action::ProposePolicy { asset, variant } => {
                let (cap, limit) = variant.limits();
                let change = OwnerChange::SetAssetPolicy {
                    asset: asset_id(asset),
                    per_tx_cap: U128(cap),
                    window_limit: U128(limit),
                    epoch_budget: variant.budget().map(U128),
                };
                let attempt = guarded(|| minder.propose_change(change));
                if model.policy_timelocked(asset, variant) {
                    propose_accepted(attempt, &mut model, Held::Policy(asset, variant))?;
                } else {
                    expect_not_timelocked(attempt, "a policy proposal")?;
                }
                Observed::Other
            }
            Action::ProposeUnfreeze => {
                let attempt = guarded(|| minder.propose_change(OwnerChange::Unfreeze));
                if model.unfreeze_timelocked() {
                    propose_accepted(attempt, &mut model, Held::Unfreeze)?;
                } else {
                    expect_not_timelocked(attempt, "an unfreeze proposal")?;
                }
                Observed::Other
            }
            Action::ExecutePending(pick) => {
                if let Some(index) = model.pending_index(pick) {
                    let entry = model.pending[index];
                    let attempt = guarded(|| {
                        let _ = minder.execute_change(U64(entry.id));
                    });
                    if model.now >= entry.ready_at {
                        // Due: it takes effect now, and not before.
                        expect_applied(attempt, "execute_change")?;
                        model.pending.remove(index);
                        match entry.change {
                            Held::Policy(asset, variant) => model.set_policy(asset, variant),
                            Held::Unfreeze => model.set_frozen(false),
                            Held::Guardian(slot) => model.guardian = slot,
                        }
                    } else {
                        let message = attempt.err().ok_or_else(|| {
                            Violation(
                                "a change was executed before its delay had passed".to_string(),
                            )
                        })?;
                        ensure!(
                            message.contains("Not ready"),
                            "an early execution was refused for another reason: {message}"
                        );
                    }
                }
                Observed::Other
            }
            Action::CancelPending(pick) => {
                if let Some(index) = model.pending_index(pick) {
                    let id = model.pending[index].id;
                    expect_applied(guarded(|| minder.cancel_change(U64(id))), "cancel_change")?;
                    model.pending.remove(index);
                }
                Observed::Other
            }
            Action::SetGuardian(guardian) => {
                let slot = guardian.slot();
                let new = slot.map(guardian_account);
                let attempt = guarded(|| minder.set_guardian(new));
                if model.guardian_timelocked(slot) {
                    expect_timelocked(attempt, "set_guardian")?;
                } else {
                    expect_applied(attempt, "set_guardian")?;
                    model.guardian = slot;
                }
                Observed::Other
            }
            Action::ProposeGuardian(guardian) => {
                let slot = guardian.slot();
                let change = OwnerChange::SetGuardian {
                    guardian: slot.map(guardian_account),
                };
                let attempt = guarded(|| minder.propose_change(change));
                if model.guardian_timelocked(slot) {
                    propose_accepted(attempt, &mut model, Held::Guardian(slot))?;
                } else {
                    expect_not_timelocked(attempt, "a guardian proposal")?;
                }
                Observed::Other
            }
            Action::Guardian { who, call } => guardian_step(&mut minder, &mut model, who, call)?,
            Action::RevokeMandate => {
                owner_call(|| minder.revoke_mandate())?;
                model.revoke();
                Observed::Other
            }
            Action::Freeze => {
                owner_call(|| minder.freeze())?;
                model.set_frozen(true);
                Observed::Other
            }
            Action::Unfreeze => {
                let attempt = guarded(|| minder.unfreeze());
                if model.unfreeze_timelocked() {
                    expect_timelocked(attempt, "unfreeze")?;
                } else {
                    expect_applied(attempt, "unfreeze")?;
                    model.set_frozen(false);
                }
                Observed::Other
            }
            Action::Advance(_) => Observed::Other,
        };
        model.settle_bookkeeping();

        check_invariants(&minder, &model, delivered, live_before)?;
        model.ledger.prune(model.now);

        let storage = persist(minder);
        // A call the guardian may not make, or that is refused to an account
        // that is not the guardian, changes no byte of stored state.
        if let Action::Guardian { who, call } = action {
            let refused = !matches!(
                call,
                GuardianCall::Freeze | GuardianCall::Revoke | GuardianCall::Cancel(_)
            ) || self.model.guardian != Some(who);
            let refused_cancel =
                matches!(observed, Observed::Guardian(what) if what.starts_with("Refused"));
            if refused || refused_cancel {
                ensure!(
                    storage == self.storage,
                    "a refused guardian call {call:?} changed the stored state"
                );
            }
        }

        Ok((World { storage, model }, observed))
    }
}

// ---- Steps ----

const GUARDIAN_ACCOUNTS: [&str; 2] = ["guardian.near", "guardian2.near"];

fn guardian_account(slot: u8) -> near_sdk::AccountId {
    account(GUARDIAN_ACCOUNTS[usize::from(slot)])
}

/// A call refused with a message containing `needle`.
fn expect_refused(result: Result<(), String>, needle: &str, what: &str) -> Result<(), Violation> {
    match result {
        Ok(()) => Err(Violation(format!("{what} was accepted"))),
        Err(message) if message.contains(needle) => Ok(()),
        Err(message) => Err(Violation(format!(
            "{what} was refused for another reason: {message}"
        ))),
    }
}

/// A call by account `who`. The model says, from the rule alone, what the
/// current guardian may do (freeze, revoke, cancel a change that is not about
/// the guardian) and that everything else, and anyone else, is refused.
fn guardian_step(
    minder: &mut Minder,
    model: &mut Model,
    who: u8,
    call: GuardianCall,
) -> Result<Observed, Violation> {
    let is_guardian = model.guardian == Some(who);
    let stranger = "Only the owner or the guardian";
    match call {
        GuardianCall::Freeze => {
            let attempt = guarded(|| minder.freeze());
            if is_guardian {
                expect_applied(attempt, "a guardian freeze")?;
                model.set_frozen(true);
                Ok(Observed::Guardian("Freeze"))
            } else {
                expect_refused(attempt, stranger, "a freeze by a non-guardian")?;
                Ok(Observed::Guardian("Refused(Freeze)"))
            }
        }
        GuardianCall::Revoke => {
            let attempt = guarded(|| minder.revoke_mandate());
            if is_guardian {
                expect_applied(attempt, "a guardian revoke")?;
                model.revoke();
                Ok(Observed::Guardian("Revoke"))
            } else {
                expect_refused(attempt, stranger, "a revoke by a non-guardian")?;
                Ok(Observed::Guardian("Refused(Revoke)"))
            }
        }
        GuardianCall::Cancel(pick) => {
            let Some(index) = model.pending_index(pick) else {
                return Ok(Observed::Other);
            };
            let entry = model.pending[index];
            let attempt = guarded(|| minder.cancel_change(U64(entry.id)));
            if !is_guardian {
                expect_refused(attempt, stranger, "a cancel by a non-guardian")?;
                Ok(Observed::Guardian("Refused(Cancel)"))
            } else if matches!(entry.change, Held::Guardian(_)) {
                expect_refused(
                    attempt,
                    "cannot cancel a change to the guardian",
                    "a guardian cancelling a change to the guardian",
                )?;
                Ok(Observed::Guardian("Refused(CancelOfGuardianChange)"))
            } else {
                expect_applied(attempt, "a guardian cancel")?;
                model.pending.remove(index);
                Ok(Observed::Guardian("Cancel"))
            }
        }
        GuardianCall::Forbidden(kind) => {
            let attempt = guarded(|| forbidden_call(minder, kind));
            expect_refused(attempt, "Only the owner can call", "a forbidden call")?;
            ensure!(
                get_created_receipts().is_empty(),
                "a forbidden guardian call {kind:?} created a promise"
            );
            Ok(Observed::Guardian("Refused(Forbidden)"))
        }
    }
}

/// An owner method with arguments that would be valid for the owner.
fn forbidden_call(minder: &mut Minder, kind: Forbidden) {
    match kind {
        Forbidden::LoosenPolicy => {
            minder.set_asset_policy(AssetId::Near, U128(80), U128(160), None);
        }
        Forbidden::Grant => {
            minder.grant_mandate(agent_key(1), U64(T0 + LONG_MANDATE), None);
        }
        Forbidden::Unfreeze => minder.unfreeze(),
        Forbidden::Withdraw => {
            let _ = minder.owner_withdraw(AssetId::Near, U128(1), account(NEAR_ALLOWED));
        }
        Forbidden::SetTreasury => minder.set_treasury(Some(account(NEAR_DISALLOWED)), None),
        Forbidden::Propose => {
            let _ = minder.propose_change(OwnerChange::Unfreeze);
        }
        Forbidden::Execute => {
            let _ = minder.execute_change(U64(1));
        }
        Forbidden::SetGuardian => minder.set_guardian(None),
    }
}

/// The owner's call was accepted and applied.
fn expect_applied(result: Result<(), String>, what: &str) -> Result<(), Violation> {
    result.map_err(|message| Violation(format!("{what} was refused: {message}")))
}

/// The call loosened authority while the timelock was on, so it had to be
/// refused as timelocked, and, being refused, changed nothing.
fn expect_timelocked(result: Result<(), String>, what: &str) -> Result<(), Violation> {
    match result {
        Ok(()) => Err(Violation(format!(
            "{what}: a loosening change took effect instantly"
        ))),
        Err(message) if message.contains("Timelocked") => Ok(()),
        Err(message) => Err(Violation(format!(
            "{what}: refused, but not as timelocked: {message}"
        ))),
    }
}

/// A proposal for something that does not loosen (or with no timelock) is
/// refused: it is called directly instead.
fn expect_not_timelocked(
    result: Result<near_sdk::json_types::U64, String>,
    what: &str,
) -> Result<(), Violation> {
    match result {
        Ok(_) => Err(Violation(format!(
            "{what} was accepted for a change that is not timelocked"
        ))),
        Err(message) if message.contains("Not timelocked") => Ok(()),
        Err(message) => Err(Violation(format!(
            "{what} was refused for another reason: {message}"
        ))),
    }
}

/// A proposal the model says is timelocked must be accepted, with the next id.
fn propose_accepted(
    result: Result<near_sdk::json_types::U64, String>,
    model: &mut Model,
    change: Held,
) -> Result<(), Violation> {
    let id = result.map_err(|message| Violation(format!("a proposal was refused: {message}")))?;
    ensure!(
        id.0 == model.next_change_id,
        "the proposal got id {}, the model expects {}",
        id.0,
        model.next_change_id
    );
    model.propose(change);
    Ok(())
}

/// An owner call that is valid by construction must not panic.
fn owner_call(call: impl FnOnce()) -> Result<(), Violation> {
    guarded(call).map_err(|message| Violation(format!("an owner call panicked: {message}")))
}

fn request(
    minder: &mut Minder,
    model: &mut Model,
    asset: Asset,
    outlay: u128,
    evm_value: u128,
    to: Recipient,
    stale_key: bool,
) -> Result<Observed, Violation> {
    // A token payment has two legs, each decided on its own: the transfer
    // (token amount, recipient, scope) and the gas (native asset's policy).
    // For every other asset there is only the transfer.
    let transfer = model.decide(asset, outlay, to, stale_key);
    let gas = (asset == Asset::Token).then(|| model.decide_gas(TOKEN_GAS, stale_key));
    // The real path reports the first leg that denies, the transfer first.
    let predicted = match (transfer, gas) {
        (Err(reason), _) => Err((reason, false)),
        (Ok(()), Some(Err(reason))) => Err((reason, true)),
        (Ok(()), _) => Ok(()),
    };
    // Monotonic revocation, from the independent ghost bookkeeping: once the
    // agent has lost authority nothing is approved, whatever `decide` says. A
    // key the mandate no longer names never has authority.
    let must_deny = model.authority_is_lost() || stale_key;
    ensure!(
        !must_deny || predicted.is_err(),
        "the model approves a request after authority was lost"
    );
    let decision = |result: Result<(), crate::DenyReason>| match result {
        Ok(()) => Decision::Allow,
        Err(reason) => Decision::Deny(reason),
    };

    // The view and the real path must agree with each other and the model.
    let agent = agent_key(if stale_key {
        model.stale_key()
    } else {
        model.mandate.key
    });
    let agent_json = near_sdk::serde_json::to_value(&agent).expect("a key is JSON");
    match asset {
        Asset::Near | Asset::Evm => {
            let viewed = if asset == Asset::Near {
                minder.dry_run(
                    agent,
                    AssetId::Near,
                    U128(outlay),
                    account(near_recipient(to)),
                )
            } else {
                minder.dry_run_evm(agent, evm_recipient(to).to_string(), U128(evm_value))
            };
            ensure!(
                viewed == decision(transfer),
                "dry_run says {viewed:?}, the model expects {:?}",
                decision(transfer)
            );
        }
        Asset::Token => {
            let viewed = minder.dry_run_evm_token(
                agent,
                TOKEN_ADDRESS.to_string(),
                evm_recipient(to).to_string(),
                U128(outlay),
            );
            let expected = crate::TokenPaymentDecision {
                token: decision(transfer),
                gas: decision(gas.expect("a token payment has a gas leg")),
            };
            ensure!(
                viewed == expected,
                "dry_run_evm_token says {viewed:?}, the model expects {expected:?}"
            );
        }
    }

    let called = guarded(|| match asset {
        Asset::Near => {
            let _ = minder.request_local_transfer(
                AssetId::Near,
                U128(outlay),
                account(near_recipient(to)),
            );
        }
        Asset::Evm => {
            let _ = minder.request_evm_payment(evm_recipient(to).to_string(), U128(evm_value));
        }
        Asset::Token => {
            let _ = minder.request_evm_token_payment(
                TOKEN_ADDRESS.to_string(),
                evm_recipient(to).to_string(),
                U128(outlay),
            );
        }
    });

    // The decision log: one event per request, allowed or denied, and a denied
    // request panicked, yet its event is in the logs. It must carry the inputs
    // and exactly the decision the model expects, leg by leg.
    let expected_event =
        expected_decision_event(asset, outlay, evm_value, to, &agent_json, transfer, gas);
    let logged = decision_events(&get_logs());
    ensure!(
        logged.len() == 1,
        "{} decision events for one request: {logged:?}",
        logged.len()
    );
    ensure!(
        logged[0] == expected_event,
        "the decision event is {}, the model expects {expected_event}",
        logged[0]
    );

    match (predicted, called) {
        (Ok(()), Ok(())) => {
            // The limits these legs were allowed under, as the contract holds them.
            let terms_of = |leg: Asset| {
                let policy = minder
                    .get_asset_policy(asset_id(leg))
                    .expect("boot sets a policy for every asset");
                (policy.window_limit.0, policy.epoch_budget.map(|b| b.0))
            };
            let mut terms = vec![terms_of(asset)];
            if asset == Asset::Token {
                terms.push(terms_of(Asset::Evm));
            }
            dispatched(model, asset, outlay, evm_value, to, terms)?;
            Ok(Observed::Allowed(asset))
        }
        (Ok(()), Err(message)) => Err(Violation(format!(
            "the contract refused a request the model allows: {message}"
        ))),
        (Err((reason, gas_leg)), Err(message)) => {
            ensure!(
                message.contains(&format!("Denied: {reason:?}")),
                "denied with the wrong reason: {message} (expected {reason:?})"
            );
            if asset == Asset::Token {
                let leg = if gas_leg {
                    "(gas, native ETH)"
                } else {
                    "(token transfer)"
                };
                ensure!(
                    message.contains(leg),
                    "denied on the wrong leg: {message} (expected {leg})"
                );
            }
            Ok(if gas_leg {
                Observed::GasDenied(reason)
            } else {
                Observed::Denied(asset, reason)
            })
        }
        (Err((reason, _)), Ok(())) => Err(Violation(format!(
            "the contract approved a request the model denies with {reason:?}{}",
            if must_deny {
                " (monotonic revocation broken: authority had been lost)"
            } else {
                ""
            }
        ))),
    }
}

/// The decision event a request must have logged, restated from the model's
/// decisions and the request's inputs alone.
fn expected_decision_event(
    asset: Asset,
    outlay: u128,
    evm_value: u128,
    to: Recipient,
    agent: &Value,
    transfer: Result<(), crate::DenyReason>,
    gas: Option<Result<(), crate::DenyReason>>,
) -> Value {
    let verdict = |result: &Result<(), crate::DenyReason>| match result {
        Ok(()) => ("allow", Value::Null),
        Err(reason) => ("deny", json!(format!("{reason:?}"))),
    };
    let leg = |name: &str, asset: Asset, amount: u128, result: &Result<(), crate::DenyReason>| {
        let (decision, reason) = verdict(result);
        json!({
            "leg": name, "asset": asset_id(asset), "amount": amount.to_string(),
            "decision": decision, "reason": reason,
        })
    };
    let (method, recipient, amount) = match asset {
        Asset::Near => (
            "request_local_transfer",
            near_recipient(to).to_string(),
            outlay,
        ),
        Asset::Evm => (
            "request_evm_payment",
            evm_recipient(to).to_string(),
            evm_value,
        ),
        Asset::Token => (
            "request_evm_token_payment",
            evm_recipient(to).to_string(),
            outlay,
        ),
    };
    // The legs evaluated: the transfer, and for a token payment the gas too
    // unless the transfer already denied.
    let mut legs = vec![leg("transfer", asset, outlay, &transfer)];
    if let Some(gas) = &gas {
        if transfer.is_ok() {
            legs.push(leg("gas", Asset::Evm, TOKEN_GAS, gas));
        }
    }
    let denial = if let Err(reason) = transfer {
        Some((reason, "transfer"))
    } else if let Some(Err(reason)) = gas {
        Some((reason, "gas"))
    } else {
        None
    };
    json!({
        "method": method,
        "agent_key": agent,
        "asset": asset_id(asset),
        "recipient": recipient,
        "amount": amount.to_string(),
        "decision": if denial.is_some() { "deny" } else { "allow" },
        "reason": denial.map(|(reason, _)| format!("{reason:?}")),
        "leg": denial.map(|(_, leg)| leg),
        "legs": legs,
    })
}

/// An allowed request must have reserved, built the right transaction, and
/// dispatched exactly what policy approved. Records it as in flight.
fn dispatched(
    model: &mut Model,
    asset: Asset,
    outlay: u128,
    evm_value: u128,
    to: Recipient,
    terms: Vec<(u128, Option<u128>)>,
) -> Result<(), Violation> {
    let receipts = get_created_receipts();
    let mut tx = None;
    let mut gas = 0;
    match asset {
        Asset::Near => {
            let callback = function_call(&receipts, CONTRACT, "on_transfer_complete");
            ensure!(
                callback.args == json!({ "asset": "Near", "amount": outlay.to_string() }),
                "local callback carries {:?}",
                callback.args
            );
            let paid = receipts
                .iter()
                .filter(|receipt| receipt.receiver_id == account(near_recipient(to)))
                .flat_map(|receipt| &receipt.actions)
                .any(|action| {
                    matches!(
                        action,
                        MockAction::Transfer { deposit, .. } if deposit.as_yoctonear() == outlay
                    )
                });
            ensure!(paid, "no native transfer of {outlay} to the recipient");
        }
        Asset::Evm | Asset::Token => {
            let sign = function_call(&receipts, SIGNER, "sign");
            let callback = function_call(&receipts, CONTRACT, "on_sign_complete");
            let (reservations, built) =
                <(Vec<(AssetId, u128)>, Eip1559Tx)>::try_from_slice(&callback.raw_args)
                    .map_err(|_| Violation("undecodable on_sign_complete arguments".to_string()))?;
            // The contract, not the caller, fixed everything but the recipient
            // and the amount: owner-set fees, the contract's nonce, and for a
            // token the transfer calldata, restated here byte by byte.
            let (expected_reservations, expected) = if asset == Asset::Evm {
                (
                    vec![(asset_id(Asset::Evm), outlay)],
                    Eip1559Tx {
                        chain_id: CHAIN_ID,
                        nonce: model.next_nonce,
                        max_priority_fee_per_gas: MAX_PRIORITY_FEE,
                        max_fee_per_gas: MAX_FEE,
                        gas_limit: GAS_LIMIT,
                        to: evm::parse_address(evm_recipient(to)).expect("fixture address"),
                        value: evm_value,
                        data: Vec::new(),
                    },
                )
            } else {
                gas = TOKEN_GAS;
                let recipient = evm::parse_address(evm_recipient(to)).expect("fixture address");
                let mut data = vec![0xa9, 0x05, 0x9c, 0xbb];
                data.extend([0u8; 12]);
                data.extend(recipient);
                data.extend([0u8; 16]);
                data.extend(outlay.to_be_bytes());
                (
                    vec![
                        (asset_id(Asset::Token), outlay),
                        (asset_id(Asset::Evm), TOKEN_GAS),
                    ],
                    Eip1559Tx {
                        chain_id: CHAIN_ID,
                        nonce: model.next_nonce,
                        max_priority_fee_per_gas: MAX_PRIORITY_FEE,
                        max_fee_per_gas: MAX_FEE,
                        gas_limit: TOKEN_GAS_LIMIT,
                        to: evm::parse_address(TOKEN_ADDRESS).expect("fixture address"),
                        value: 0,
                        data,
                    },
                )
            };
            ensure!(
                reservations == expected_reservations,
                "the callback carries reservations {reservations:?}, expected {expected_reservations:?}"
            );
            ensure!(built == expected, "built {built:?}, expected {expected:?}");
            // Hash integrity: what the signer is asked to sign is the hash of
            // exactly that transaction, data included.
            ensure!(
                sign.args["request"]["payload_v2"]["Ecdsa"]
                    == evm::hex_encode(&evm::signing_hash(&built)),
                "the signer was sent a different hash than the transaction's"
            );
            ensure!(
                sign.args["request"]["path"] == DERIVATION_PATH,
                "wrong derivation path"
            );
            // Nonces are strictly increasing and never reused.
            ensure!(
                model.issued.insert((built.chain_id, built.nonce)),
                "the pair (chain {}, nonce {}) was issued twice",
                built.chain_id,
                built.nonce
            );
            model.next_nonce += 1;
            tx = Some(built);
        }
    }
    let pending = Pending {
        asset,
        outlay,
        gas,
        tx,
        terms,
    };
    model.reserve_pending(&pending);
    model.queue.push(pending);
    Ok(())
}

fn deliver(
    minder: &mut Minder,
    model: &mut Model,
    index: usize,
    outcome: Outcome,
) -> Result<Observed, Violation> {
    let pending = model.queue[index].clone();
    let tx = pending.tx.clone();
    let called = guarded(|| match pending.asset {
        Asset::Near => minder.on_transfer_complete(AssetId::Near, U128(pending.outlay)),
        Asset::Evm | Asset::Token => minder.on_sign_complete(
            pending
                .legs()
                .into_iter()
                .map(|(asset, amount)| (asset_id(asset), amount))
                .collect(),
            tx.expect("an EVM request carries its transaction"),
        ),
    });
    // A callback may never panic: that would strand the reservation.
    ensure!(
        called.is_ok(),
        "the callback panicked: {}",
        called.err().unwrap_or_default()
    );

    let logs = get_logs();
    let events: Vec<&String> = logs
        .iter()
        .filter(|line| line.starts_with("EVENT_JSON:"))
        .collect();
    match pending.asset {
        Asset::Near => ensure!(events.is_empty(), "a local settlement emitted an event"),
        Asset::Evm | Asset::Token => {
            // The signed transaction is emitted iff the signature assembles and
            // recovers to the configured sender: only `Ok` delivers such a signature.
            ensure!(
                events.len() == usize::from(outcome == Outcome::Ok),
                "{outcome:?} produced {} events",
                events.len()
            );
            if outcome == Outcome::Ok {
                let tx = pending.tx.as_ref().expect("EVM transaction");
                let event: Value = near_sdk::serde_json::from_str(
                    events[0].strip_prefix("EVENT_JSON:").unwrap_or_default(),
                )
                .map_err(|_| Violation("unparseable event".to_string()))?;
                // The reference serialization by the independent signer.
                let reference = fixture_signature(&evm::signing_hash(tx), Signer::Sender).raw_tx;
                ensure!(
                    event["data"][0]["nonce"] == tx.nonce
                        && event["data"][0]["raw_tx"]
                            == format!("0x{}", evm::hex_encode(&reference)),
                    "the event does not carry the built transaction"
                );
            } else if outcome == Outcome::OtherSigner {
                ensure!(
                    logs.iter().any(|l| l.contains("authenticity failure")),
                    "a signature by another key was not logged as an authenticity failure"
                );
            } else if outcome.books_spend() {
                ensure!(
                    logs.iter()
                        .any(|l| l.contains("booked, no transaction emitted")),
                    "an unusable-but-successful result was not logged as booked"
                );
            } else {
                ensure!(
                    logs.iter().any(|l| l.contains("not signed")),
                    "a failed receipt was not logged"
                );
            }
        }
    }
    model.deliver(index, outcome);
    // Booked now, at settlement, under the contract's own current epoch: this
    // is the record the loss-bound check works from.
    if outcome.books_spend() {
        let epoch = minder.get_mandate().expect("boot grants a mandate").epoch;
        for ((asset, amount), (limit, budget)) in pending.legs().into_iter().zip(&pending.terms) {
            model.ledger.book(
                asset.index(),
                epoch,
                Booking {
                    time: model.now,
                    amount,
                    limit: *limit,
                    budget: *budget,
                },
            );
        }
    }
    Ok(Observed::Delivered(pending.asset, outcome))
}

// ---- Invariants ----

/// `delivered` is, per asset, what a delivery in this step should have added
/// to the window total (zero for a failed receipt). A token payment adds to
/// two assets.
fn check_invariants(
    minder: &Minder,
    model: &Model,
    delivered: [u128; ASSET_COUNT],
    live_before: [u128; ASSET_COUNT],
) -> Result<(), Violation> {
    let mut in_flight = [0u128; ASSET_COUNT];
    for pending in &model.queue {
        for (asset, amount) in pending.legs() {
            in_flight[asset.index()] += amount;
        }
    }
    for asset in ASSETS {
        let i = asset.index();
        let live = live_spend(minder, asset);
        let reserved = minder.get_reserved(asset_id(asset)).0;

        // The contract's accounting equals the oracle's, which also means
        // nothing is booked without a matching allowed request, and every
        // reservation was released exactly once.
        ensure!(
            live == model.live_committed(asset),
            "{asset:?}: window spend is {live}, the model has {}",
            model.live_committed(asset)
        );
        ensure!(
            reserved == model.reserved[i] && reserved == in_flight[i],
            "{asset:?}: reserved is {reserved}, the model has {} (in flight {})",
            model.reserved[i],
            in_flight[i]
        );

        // The window never lets counted spend pass the limit, except while an
        // owner-lowered limit is still above what was already counted.
        let effective = live.saturating_add(reserved);
        ensure!(
            effective <= model.policy[i].limit || model.lowered[i],
            "{asset:?}: effective spend {effective} passed the limit {}",
            model.policy[i].limit
        );

        // The lifetime budget. The contract's per-epoch counter (read through
        // its own lazy-reset accessor, so a stale entry from an earlier grant
        // must read as zero) equals the model's, and usage, which counts what
        // is in flight, never passes the budget except while the owner has
        // lowered it below what the grant already used.
        let epoch_committed = minder.epoch_committed(&asset_id(asset));
        ensure!(
            epoch_committed == model.epoch_committed[i],
            "{asset:?}: this grant's committed spend is {epoch_committed}, the model has {}",
            model.epoch_committed[i]
        );
        if let Some(budget) = model.policy[i].budget {
            ensure!(
                model.epoch_usage(asset) <= budget || model.budget_lowered[i],
                "{asset:?}: budget usage {} passed the budget {budget}",
                model.epoch_usage(asset)
            );
        }

        // Spend appears only when a callback is delivered, and then by exactly
        // the delivered outlay. The clock is fixed within a step (an `Advance`
        // moves it before `live_before` is read), so nothing ages out here.
        let added = delivered[i];
        ensure!(
            live == live_before[i].saturating_add(added),
            "{asset:?}: spend went from {} to {live}, expected +{added}",
            live_before[i]
        );
    }

    if model.queue.is_empty() {
        for asset in ASSETS {
            ensure!(
                minder.get_reserved(asset_id(asset)).0 == 0,
                "{asset:?}: a reservation outlived every callback"
            );
        }
    }

    let nonce = minder.get_evm_nonce(CHAIN_ID).0;
    ensure!(
        nonce == model.next_nonce,
        "the nonce counter is {nonce}, the model has {}",
        model.next_nonce
    );

    ensure!(
        (minder.get_status() == Status::Frozen) == model.frozen,
        "frozen state differs from the model"
    );
    // Every asset's policy is exactly what the model says. A loosening that took
    // effect early, or a tightening that did not, shows here at once.
    for asset in ASSETS {
        let stored = minder
            .get_asset_policy(asset_id(asset))
            .expect("boot sets a policy for every asset");
        let policy = model.policy[asset.index()];
        ensure!(
            (
                stored.per_tx_cap.0,
                stored.window_limit.0,
                stored.epoch_budget.map(|b| b.0)
            ) == (policy.cap, policy.limit, policy.budget),
            "{asset:?}: the stored policy {stored:?} differs from the model's {policy:?}"
        );
    }
    // The timelock: the pending changes, their ids and their ready-at times,
    // are exactly the model's, and the delay is what was set.
    ensure!(
        minder.get_config().timelock_delay.0 == model.delay,
        "the timelock delay differs from the model"
    );
    ensure!(
        minder.get_config().guardian == model.guardian.map(guardian_account),
        "the guardian differs from the model"
    );
    let pending = minder.get_pending_changes(None, None);
    ensure!(
        pending.len() == model.pending.len(),
        "{} pending changes, the model has {}",
        pending.len(),
        model.pending.len()
    );
    for (stored, held) in pending.iter().zip(&model.pending) {
        let expected = match held.change {
            Held::Policy(asset, variant) => {
                let (cap, limit) = variant.limits();
                OwnerChange::SetAssetPolicy {
                    asset: asset_id(asset),
                    per_tx_cap: U128(cap),
                    window_limit: U128(limit),
                    epoch_budget: variant.budget().map(U128),
                }
            }
            Held::Unfreeze => OwnerChange::Unfreeze,
            Held::Guardian(slot) => OwnerChange::SetGuardian {
                guardian: slot.map(guardian_account),
            },
        };
        ensure!(
            stored.id.0 == held.id
                && stored.ready_at.0 == held.ready_at
                && stored.change == expected,
            "pending change {stored:?} differs from the model's {held:?}"
        );
    }
    // The user-facing guarantee, from the record of what was booked alone.
    model.ledger.check().map_err(Violation)?;
    let mandate = minder.get_mandate().expect("boot grants a mandate");
    ensure!(
        mandate.epoch == model.mandate.epoch
            && mandate.revoked == model.mandate.revoked
            && mandate.expiry.0 == model.mandate.expiry
            && mandate.scope == stored_scope(model.mandate.scope),
        "mandate {mandate:?} differs from the model {:?}",
        model.mandate
    );
    Ok(())
}

/// What the owner passes to `grant_mandate` for a harness scope. `Unscoped`
/// passes nothing at all, the way a call that predates scopes would.
fn grant_scope(scope: Scope) -> Option<MandateScope> {
    let only = MandateScope::default;
    match scope {
        Scope::Unscoped => None,
        Scope::NearOnly => Some(MandateScope {
            allowed_assets: Some(vec![AssetId::Near]),
            ..only()
        }),
        Scope::AllowedRecipientsOnly => Some(MandateScope {
            allowed_near_recipients: Some(vec![account(NEAR_ALLOWED)]),
            allowed_evm_recipients: Some(vec![EVM_ALLOWED.to_string()]),
            ..only()
        }),
        Scope::DisallowedRecipientsOnly => Some(MandateScope {
            allowed_near_recipients: Some(vec![account(NEAR_DISALLOWED)]),
            allowed_evm_recipients: Some(vec![EVM_DISALLOWED.to_string()]),
            ..only()
        }),
        Scope::CapTight => Some(MandateScope {
            per_tx_cap: Some(U128(40)),
            ..only()
        }),
        Scope::CapLoose => Some(MandateScope {
            per_tx_cap: Some(U128(100)),
            ..only()
        }),
    }
}

/// The scope the contract should have stored for it.
fn stored_scope(scope: Scope) -> MandateScope {
    grant_scope(scope).unwrap_or_default()
}

// ---- Environment and helpers ----

fn asset_id(asset: Asset) -> AssetId {
    match asset {
        Asset::Near => AssetId::Near,
        Asset::Evm => AssetId::Evm(CHAIN_ID),
        Asset::Token => AssetId::Evm20 {
            chain_id: CHAIN_ID,
            token: evm::parse_address(TOKEN_ADDRESS).expect("fixture address"),
        },
    }
}

fn near_recipient(to: Recipient) -> &'static str {
    match to {
        Recipient::Allowed => NEAR_ALLOWED,
        Recipient::Disallowed => NEAR_DISALLOWED,
    }
}

fn evm_recipient(to: Recipient) -> &'static str {
    match to {
        Recipient::Allowed => EVM_ALLOWED,
        Recipient::Disallowed => EVM_DISALLOWED,
    }
}

fn live_spend(minder: &Minder, asset: Asset) -> u128 {
    minder.get_live_spend(asset_id(asset)).0
}

/// One of the two keys the harness alternates the agent between.
fn agent_key(index: u8) -> PublicKey {
    PublicKey::from_parts(near_sdk::CurveType::ED25519, vec![index + 1; 32]).unwrap()
}

fn context(predecessor: &str, now: u64, signer_key: Option<PublicKey>) -> VMContext {
    let mut builder = VMContextBuilder::new();
    builder
        .current_account_id(account(CONTRACT))
        .predecessor_account_id(account(predecessor))
        .block_timestamp(now)
        .prepaid_gas(Gas::from_tgas(300));
    if let Some(key) = signer_key {
        builder
            .signer_account_id(account(CONTRACT))
            .signer_account_pk(key);
    }
    builder.build()
}

thread_local! {
    /// Builds a mocked environment. The runtime configuration is parsed from
    /// embedded parameter tables, which dominates the cost of a step, so it
    /// is built once per thread and cloned.
    static NEW_ENV: Box<dyn Fn(VMContext, Vec<PromiseResult>, Storage) -> MockedBlockchain> = {
        let config = near_sdk::test_vm_config();
        let fees = RuntimeFeesConfig::test();
        Box::new(move |context, promise_results, storage| {
            MockedBlockchain::new(
                context,
                config.clone(),
                fees.clone(),
                promise_results,
                storage,
                HashMap::new(),
                None,
            )
        })
    };
}

fn set_env(context: VMContext, promise_results: Vec<PromiseResult>, storage: Storage) {
    let blockchain = NEW_ENV.with(|new_env| new_env(context, promise_results, storage));
    env::set_blockchain_interface(blockchain);
}

/// Writes the contract state, drops it (which flushes the cached
/// collections), and takes the whole stored state.
fn persist(minder: Minder) -> Storage {
    env::state_write(&minder);
    drop(minder);
    near_sdk::mock::with_mocked_blockchain(|blockchain| blockchain.take_storage())
}

fn promise_result(pending: &Pending, outcome: Outcome) -> PromiseResult {
    match (pending.asset, outcome) {
        (_, Outcome::Fail) => PromiseResult::Failed,
        (Asset::Near, Outcome::Ok) => PromiseResult::Successful(Vec::new()),
        // A local transfer that succeeded but returned data.
        (Asset::Near, _) => PromiseResult::Successful(vec![1, 2, 3]),
        (Asset::Evm | Asset::Token, Outcome::Ok) => {
            PromiseResult::Successful(signature(pending, Signer::Sender))
        }
        // Well formed and genuine, over the right hash, but by a key that is not the sender.
        (Asset::Evm | Asset::Token, Outcome::OtherSigner) => {
            PromiseResult::Successful(signature(pending, Signer::Other))
        }
        (Asset::Evm | Asset::Token, Outcome::WrongShape) => PromiseResult::Successful(
            json!({ "big_r": format!("02{R}"), "s": S, "recovery_id": 1 })
                .to_string()
                .into_bytes(),
        ),
        (Asset::Evm | Asset::Token, Outcome::Garbage) => {
            PromiseResult::Successful(b"\xff\xfe not a signature".to_vec())
        }
        // A genuine signature padded past the 4096-byte read buffer.
        (Asset::Evm | Asset::Token, Outcome::Oversized) => {
            let mut value: Value =
                near_sdk::serde_json::from_slice(&signature(pending, Signer::Sender))
                    .expect("fixture signature is JSON");
            value["padding"] = json!("x".repeat(5000));
            PromiseResult::Successful(value.to_string().into_bytes())
        }
    }
}

/// A real signature by `signer` over the hash of the pending request's transaction.
fn signature(pending: &Pending, signer: Signer) -> Vec<u8> {
    let tx = pending
        .tx
        .as_ref()
        .expect("an EVM request carries its transaction");
    signer_response(&fixture_signature(&evm::signing_hash(tx), signer))
}

thread_local! {
    static QUIET: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f`, turning a panic into its message. Expected denials panic by
/// design, so the default hook is silenced for this thread while `f` runs;
/// other threads' panics still print.
pub fn guarded<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if !QUIET.with(Cell::get) {
                previous(info);
            }
        }));
    });
    QUIET.with(|quiet| quiet.set(true));
    let result = panic::catch_unwind(AssertUnwindSafe(f));
    QUIET.with(|quiet| quiet.set(false));
    result.map_err(|payload| {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
            .unwrap_or_else(|| "a panic with a non-string payload".to_string())
    })
}
