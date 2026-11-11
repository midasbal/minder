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
use near_sdk::{env, Gas, MockedBlockchain, PromiseResult, RuntimeFeesConfig, VMContext};

use crate::evm::{self, Eip1559Tx};
use crate::test_support::{account, function_call};
use crate::types::{Decision, Status};
use crate::{AssetId, Minder};

use super::actions::{Action, Asset, Outcome, Pick, Recipient, ASSETS};
use super::model::{Model, Pending};
use super::{
    CHAIN_ID, DERIVATION_PATH, EVM_ALLOWED, EVM_DISALLOWED, GAS_LIMIT, LONG_MANDATE, MAX_FEE,
    MAX_PRIORITY_FEE, NEAR_ALLOWED, NEAR_DISALLOWED, NUM_BUCKETS, T0, WINDOW_LEN,
};

const AGENT: &str = "agent.near";
const OWNER: &str = "owner.near";
const CONTRACT: &str = "minder.near";
const SIGNER: &str = "signer.near";

/// A valid signature (low `s`) as the signer returns it.
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
    Denied(Asset, crate::DenyReason),
    Delivered(Asset, Outcome),
    Other,
}

impl Observed {
    /// A coverage key: what happened, ignoring amounts.
    pub fn key(self) -> String {
        match self {
            Observed::Allowed(asset) => format!("Allowed({asset:?})"),
            Observed::Denied(asset, reason) => format!("Denied({asset:?}, {reason:?})"),
            Observed::Delivered(asset, outcome) => format!("Delivered({asset:?}, {outcome:?})"),
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
    /// Initialized, armed for both assets, with a long mandate.
    pub fn boot() -> World {
        set_env(context(OWNER, T0), Vec::new(), HashMap::new());
        let mut minder = Minder::init(account(OWNER), U64(WINDOW_LEN), NUM_BUCKETS);
        for asset in ASSETS {
            let policy = super::model::Policy::of(super::actions::PolicyVariant::Default);
            minder.set_asset_policy(asset_id(asset), U128(policy.cap), U128(policy.limit));
        }
        minder.set_recipient(account(NEAR_ALLOWED), true);
        minder.set_evm_recipient(EVM_ALLOWED.to_string(), true);
        minder.set_evm_config(
            account(SIGNER),
            DERIVATION_PATH.to_string(),
            CHAIN_ID,
            U128(MAX_FEE),
            U128(MAX_PRIORITY_FEE),
            Some(GAS_LIMIT),
        );
        minder.grant_mandate(account(AGENT), U64(T0 + LONG_MANDATE));
        World {
            storage: persist(minder),
            model: Model::new(T0, T0 + LONG_MANDATE),
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
        let delivered = delivery.map(|(index, outcome)| {
            let pending = &model.queue[index];
            (
                pending.asset,
                if outcome.books_spend() {
                    pending.outlay
                } else {
                    0
                },
            )
        });
        let (caller, promise_results) = match (action, delivery) {
            (Action::Request { .. } | Action::Advance(_), _) | (Action::Deliver { .. }, None) => {
                (AGENT, Vec::new())
            }
            (Action::Deliver { .. }, Some((index, outcome))) => {
                (CONTRACT, vec![promise_result(&model.queue[index], outcome)])
            }
            _ => (OWNER, Vec::new()),
        };

        set_env(
            context(caller, model.now),
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
            )?,
            Action::Deliver { .. } => match delivery {
                Some((index, outcome)) => deliver(&mut minder, &mut model, index, outcome)?,
                None => Observed::Other,
            },
            Action::SetPolicy { asset, variant } => {
                let (cap, limit) = variant.limits();
                owner_call(|| minder.set_asset_policy(asset_id(asset), U128(cap), U128(limit)))?;
                model.set_policy(asset, variant);
                Observed::Other
            }
            Action::GrantMandate(grant) => {
                let expiry = model.now + grant.lifetime();
                owner_call(|| minder.grant_mandate(account(AGENT), U64(expiry)))?;
                model.grant(expiry);
                Observed::Other
            }
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
                owner_call(|| minder.unfreeze())?;
                model.set_frozen(false);
                Observed::Other
            }
            Action::Advance(_) => Observed::Other,
        };
        model.settle_bookkeeping();

        check_invariants(&minder, &model, delivered, live_before)?;

        Ok((
            World {
                storage: persist(minder),
                model,
            },
            observed,
        ))
    }
}

// ---- Steps ----

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
) -> Result<Observed, Violation> {
    let predicted = model.decide(asset, outlay, to);
    // Monotonic revocation, from the independent ghost bookkeeping: once the
    // agent has lost authority nothing is approved, whatever `decide` says.
    let must_deny = model.authority_is_lost();
    ensure!(
        !must_deny || predicted.is_err(),
        "the model approves a request after authority was lost"
    );
    let expected = match predicted {
        Ok(()) => Decision::Allow,
        Err(reason) => Decision::Deny(reason),
    };

    // The view and the real path must agree with each other and the model.
    let agent = account(AGENT);
    let viewed = match asset {
        Asset::Near => minder.dry_run(
            agent,
            AssetId::Near,
            U128(outlay),
            account(near_recipient(to)),
        ),
        Asset::Evm => minder.dry_run_evm(agent, evm_recipient(to).to_string(), U128(evm_value)),
    };
    ensure!(
        viewed == expected,
        "dry_run says {viewed:?}, the model expects {expected:?}"
    );

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
    });

    match (predicted, called) {
        (Ok(()), Ok(())) => {
            dispatched(model, asset, outlay, evm_value, to)?;
            Ok(Observed::Allowed(asset))
        }
        (Ok(()), Err(message)) => Err(Violation(format!(
            "the contract refused a request the model allows: {message}"
        ))),
        (Err(reason), Err(message)) => {
            ensure!(
                message.contains(&format!("Denied: {reason:?}")),
                "denied with the wrong reason: {message} (expected {reason:?})"
            );
            Ok(Observed::Denied(asset, reason))
        }
        (Err(reason), Ok(())) => Err(Violation(format!(
            "the contract approved a request the model denies with {reason:?}{}",
            if must_deny {
                " (monotonic revocation broken: authority had been lost)"
            } else {
                ""
            }
        ))),
    }
}

/// An allowed request must have reserved, built the right transaction, and
/// dispatched exactly what policy approved. Records it as in flight.
fn dispatched(
    model: &mut Model,
    asset: Asset,
    outlay: u128,
    evm_value: u128,
    to: Recipient,
) -> Result<(), Violation> {
    let receipts = get_created_receipts();
    let mut tx = None;
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
        Asset::Evm => {
            let sign = function_call(&receipts, SIGNER, "sign");
            let callback = function_call(&receipts, CONTRACT, "on_sign_complete");
            let (callback_asset, callback_outlay, built) =
                <(AssetId, u128, Eip1559Tx)>::try_from_slice(&callback.raw_args)
                    .map_err(|_| Violation("undecodable on_sign_complete arguments".to_string()))?;
            ensure!(
                callback_asset == AssetId::Evm(CHAIN_ID),
                "wrong callback asset"
            );
            ensure!(
                callback_outlay == outlay,
                "callback outlay {callback_outlay}"
            );
            // The contract, not the caller, fixed everything but to and value.
            let expected = Eip1559Tx {
                chain_id: CHAIN_ID,
                nonce: model.next_nonce,
                max_priority_fee_per_gas: MAX_PRIORITY_FEE,
                max_fee_per_gas: MAX_FEE,
                gas_limit: GAS_LIMIT,
                to: evm::parse_address(evm_recipient(to)).expect("fixture address"),
                value: evm_value,
            };
            ensure!(built == expected, "built {built:?}, expected {expected:?}");
            // Hash integrity: what the signer is asked to sign is the hash of
            // exactly that transaction.
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
    model.reserve(asset, outlay);
    model.queue.push(Pending { asset, outlay, tx });
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
        Asset::Evm => minder.on_sign_complete(
            AssetId::Evm(CHAIN_ID),
            pending.outlay,
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
        Asset::Evm => {
            // The signed transaction is emitted for a valid signature only.
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
                let raw = evm::encode_signed(tx, hex32(R), hex32(S), 1);
                ensure!(
                    event["data"][0]["nonce"] == tx.nonce
                        && event["data"][0]["raw_tx"] == format!("0x{}", evm::hex_encode(&raw)),
                    "the event does not carry the built transaction"
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
    Ok(Observed::Delivered(pending.asset, outcome))
}

// ---- Invariants ----

/// `delivered` is the asset and amount a delivery in this step should have
/// added to the window total (zero for a failed receipt).
fn check_invariants(
    minder: &Minder,
    model: &Model,
    delivered: Option<(Asset, u128)>,
    live_before: [u128; 2],
) -> Result<(), Violation> {
    let mut in_flight = [0u128; 2];
    for pending in &model.queue {
        in_flight[pending.asset.index()] += pending.outlay;
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

        // Spend appears only when a callback is delivered, and then by exactly
        // the delivered outlay. The clock is fixed within a step (an `Advance`
        // moves it before `live_before` is read), so nothing ages out here.
        let added = match delivered {
            Some((delivered_asset, amount)) if delivered_asset == asset => amount,
            _ => 0,
        };
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
    let mandate = minder.get_mandate().expect("boot grants a mandate");
    ensure!(
        mandate.epoch == model.mandate.epoch
            && mandate.revoked == model.mandate.revoked
            && mandate.expiry.0 == model.mandate.expiry,
        "mandate {mandate:?} differs from the model {:?}",
        model.mandate
    );
    Ok(())
}

// ---- Environment and helpers ----

fn asset_id(asset: Asset) -> AssetId {
    match asset {
        Asset::Near => AssetId::Near,
        Asset::Evm => AssetId::Evm(CHAIN_ID),
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

fn hex32(text: &str) -> [u8; 32] {
    evm::hex_decode(text)
        .and_then(|bytes| bytes.try_into().ok())
        .expect("fixture hex")
}

fn context(predecessor: &str, now: u64) -> VMContext {
    let mut builder = VMContextBuilder::new();
    builder
        .current_account_id(account(CONTRACT))
        .predecessor_account_id(account(predecessor))
        .block_timestamp(now)
        .prepaid_gas(Gas::from_tgas(300));
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
        (Asset::Evm, Outcome::Ok) => PromiseResult::Successful(signature(1)),
        (Asset::Evm, Outcome::WrongShape) => PromiseResult::Successful(
            json!({ "big_r": format!("02{R}"), "s": S, "recovery_id": 1 })
                .to_string()
                .into_bytes(),
        ),
        (Asset::Evm, Outcome::Garbage) => {
            PromiseResult::Successful(b"\xff\xfe not a signature".to_vec())
        }
        // A genuine signature padded past the 4096-byte read buffer.
        (Asset::Evm, Outcome::Oversized) => {
            let mut value: Value =
                near_sdk::serde_json::from_slice(&signature(1)).expect("fixture signature is JSON");
            value["padding"] = json!("x".repeat(5000));
            PromiseResult::Successful(value.to_string().into_bytes())
        }
    }
}

fn signature(recovery_id: u8) -> Vec<u8> {
    json!({
        "scheme": "Secp256k1",
        "big_r": { "affine_point": format!("02{R}") },
        "s": { "scalar": S },
        "recovery_id": recovery_id,
    })
    .to_string()
    .into_bytes()
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
