//! Stateful, model-based invariant harness.
//!
//! A shadow [`model::Model`] states what the contract should do. A
//! [`world::World`] pairs the real contract's persisted state with the model;
//! `World::apply` runs one [`actions::Action`] against the real contract under
//! the mocked environment and checks every invariant. Two drivers feed it
//! action sequences: [`explore`] enumerates every sequence up to a depth over a
//! reduced alphabet, [`random`] draws long sequences over the full alphabet
//! from a fixed seed. Async requests go on an in-flight queue whose callbacks
//! are delivered later, in either order, with every result the signer or token
//! can produce, so reservations, exactly-once settlement and TOCTOU are
//! exercised across interleavings.
//!
//! Unit level: no network, deterministic, test-only.

mod actions;
mod explore;
mod model;
mod random;
mod world;

pub(crate) use random::seeded_runner;

use actions::{
    deliver, request, Action, Amount, Asset, Grant, Outcome, Pick, PolicyVariant, Recipient, Tick,
};
use world::World;

// ---- Fixed scenario ----

/// A 400 ns window in four 100 ns slots.
pub(crate) const NUM_BUCKETS: u32 = 4;
pub(crate) const SLOT_LEN: u64 = 100;
pub(crate) const WINDOW_LEN: u64 = SLOT_LEN * NUM_BUCKETS as u64;
/// Slot aligned, so the time advances land on known positions in a slot.
pub(crate) const T0: u64 = 10_000;
pub(crate) const LONG_MANDATE: u64 = 100_000;

pub(crate) const CHAIN_ID: u64 = 5;
pub(crate) const DERIVATION_PATH: &str = "model-path";
/// Tiny fee numbers keep every outlay in the same small range as the NEAR
/// policy: the fee is `GAS_LIMIT * MAX_FEE` = 10.
pub(crate) const MAX_FEE: u128 = 5;
pub(crate) const MAX_PRIORITY_FEE: u128 = 1;
pub(crate) const GAS_LIMIT: u64 = 2;
pub(crate) const FEE_COST: u128 = MAX_FEE * GAS_LIMIT as u128;

pub(crate) const NEAR_ALLOWED: &str = "bob.near";
pub(crate) const NEAR_DISALLOWED: &str = "mallory.near";
pub(crate) const EVM_ALLOWED: &str = "0x70997970c51812dc3a010c7d01b50e0d17dc79c8";
pub(crate) const EVM_DISALLOWED: &str = "0x2222222222222222222222222222222222222222";

// ---- Exhaustive suites ----

/// Search depth. Three suites, each over a reduced alphabet chosen so that the
/// reachable state space at this depth stays searchable.
const DEPTH: usize = 8;

fn run_exhaustive(name: &str, alphabet: &[Action], depth: usize) -> explore::Stats {
    let started = std::time::Instant::now();
    let stats = match explore::explore(&World::boot(), alphabet, depth) {
        Ok(stats) => stats,
        Err(failure) => panic!("[{name}] invariant violated: {failure}"),
    };
    println!(
        "[{name}] depth {depth}, {} actions: {} states, {} transitions, {} deduplicated, \
         {} no-ops skipped, {:.1}s",
        alphabet.len(),
        stats.states,
        stats.transitions,
        stats.deduplicated,
        stats.skipped_noops,
        started.elapsed().as_secs_f64()
    );
    stats
}

fn assert_covered(coverage: &std::collections::BTreeSet<String>, expected: &[&str]) {
    for key in expected {
        assert!(
            coverage.contains(*key),
            "the driver never produced {key}; it is not exercising what it claims to \
             (seen: {coverage:?})"
        );
    }
}

#[test]
fn exhaustive_accounting_across_interleavings() {
    // Two assets, requests at and below the cap that together press the
    // limit, and every kind of delivery in either order: TOCTOU, exactly-once
    // release, and the G1 booking rule.
    let alphabet = [
        request(Asset::Near, Amount::AtCap),
        request(Asset::Near, Amount::Mid),
        request(Asset::Evm, Amount::AtCap),
        request(Asset::Evm, Amount::Mid),
        deliver(Pick::Oldest, Outcome::Ok),
        deliver(Pick::Newest, Outcome::Fail),
        deliver(Pick::Oldest, Outcome::WrongShape),
        deliver(Pick::Newest, Outcome::Oversized),
        Action::Advance(Tick::Window),
    ];
    let stats = run_exhaustive("accounting", &alphabet, DEPTH);
    assert_covered(
        &stats.coverage,
        &[
            "Allowed(Near)",
            "Allowed(Evm)",
            "Denied(Near, WindowLimitExceeded)",
            "Denied(Evm, WindowLimitExceeded)",
            "Delivered(Near, Ok)",
            "Delivered(Evm, Ok)",
            "Delivered(Evm, Fail)",
            "Delivered(Evm, WrongShape)",
            "Delivered(Evm, Oversized)",
        ],
    );
}

#[test]
fn exhaustive_authority_and_time() {
    // Freeze, revoke, expiry and re-grant interleaved with in-flight requests
    // and the clock: monotonic revocation, and settlement after authority is
    // gone.
    let alphabet = [
        request(Asset::Near, Amount::Mid),
        request(Asset::Evm, Amount::Mid),
        deliver(Pick::Oldest, Outcome::Ok),
        deliver(Pick::Oldest, Outcome::Fail),
        Action::Freeze,
        Action::Unfreeze,
        Action::RevokeMandate,
        Action::GrantMandate(Grant::Short),
        Action::GrantMandate(Grant::Long),
        Action::Advance(Tick::Slot),
    ];
    let stats = run_exhaustive("authority", &alphabet, DEPTH);
    assert_covered(
        &stats.coverage,
        &[
            "Allowed(Near)",
            "Allowed(Evm)",
            "Denied(Near, Frozen)",
            "Denied(Evm, MandateRevoked)",
            "Denied(Near, MandateExpired)",
        ],
    );
}

#[test]
fn exhaustive_policy_changes_and_caps() {
    // The owner tightening and loosening limits under in-flight spend, caps,
    // and a recipient that is not allowlisted.
    let alphabet = [
        request(Asset::Near, Amount::AtCap),
        request(Asset::Evm, Amount::Mid),
        request(Asset::Near, Amount::OverCap),
        Action::Request {
            asset: Asset::Evm,
            amount: Amount::Mid,
            to: Recipient::Disallowed,
        },
        Action::SetPolicy {
            asset: Asset::Near,
            variant: PolicyVariant::Tight,
        },
        Action::SetPolicy {
            asset: Asset::Near,
            variant: PolicyVariant::Loose,
        },
        Action::SetPolicy {
            asset: Asset::Evm,
            variant: PolicyVariant::Tight,
        },
        Action::SetPolicy {
            asset: Asset::Near,
            variant: PolicyVariant::Default,
        },
        deliver(Pick::Oldest, Outcome::Ok),
        deliver(Pick::Newest, Outcome::OtherSigner),
        Action::Advance(Tick::Slot),
    ];
    let stats = run_exhaustive("policy", &alphabet, DEPTH);
    assert_covered(
        &stats.coverage,
        &[
            "Denied(Near, PerTxCapExceeded)",
            "Denied(Evm, RecipientNotAllowed)",
            "Delivered(Evm, OtherSigner)",
        ],
    );
}

// ---- Random driver ----

const RANDOM_CASES: u32 = 4_000;
const RANDOM_MAX_LEN: usize = 40;

/// `MINDER_MODEL_CASES` overrides the number of random cases, for a longer
/// run than the default.
fn random_cases() -> u32 {
    std::env::var("MINDER_MODEL_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(RANDOM_CASES)
}

#[test]
fn random_long_sequences() {
    let started = std::time::Instant::now();
    let cases = random_cases();
    let coverage = match random::run(&World::boot(), cases, RANDOM_MAX_LEN) {
        Ok(coverage) => coverage,
        Err(failure) => panic!("invariant violated: {failure}"),
    };
    // Every kind of step the random walk is meant to reach, reached.
    assert_covered(
        &coverage,
        &[
            "Allowed(Near)",
            "Allowed(Evm)",
            "Denied(Near, WindowLimitExceeded)",
            "Denied(Evm, PerTxCapExceeded)",
            "Denied(Near, RecipientNotAllowed)",
            "Denied(Evm, Frozen)",
            "Denied(Near, MandateRevoked)",
            "Denied(Evm, MandateExpired)",
            "Delivered(Near, Ok)",
            "Delivered(Near, Fail)",
            "Delivered(Evm, Ok)",
            "Delivered(Evm, Fail)",
            "Delivered(Evm, WrongShape)",
            "Delivered(Evm, Garbage)",
            "Delivered(Evm, Oversized)",
            "Delivered(Evm, OtherSigner)",
        ],
    );
    println!(
        "[random] {cases} cases of up to {RANDOM_MAX_LEN} actions, seed fixed, {:.1}s",
        started.elapsed().as_secs_f64()
    );
}
