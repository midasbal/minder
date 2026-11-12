//! Random driver: long sequences over the full alphabet, from a fixed seed so
//! every run draws the same cases. A failure is shrunk by proptest to a
//! minimal sequence.

use std::cell::RefCell;
use std::collections::BTreeSet;

use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestError, TestRng, TestRunner};

use super::actions::{full_alphabet, Action};
use super::explore::report;
use super::world::{guarded, World};

/// Fixed so that a given build always runs the same cases.
const SEED: [u8; 32] = *b"minder-model-based-invariants-01";

/// A runner with the fixed seed, shared with the byte-level fuzz tests so every
/// property test in the crate is reproducible.
pub(crate) fn seeded_runner(cases: u32) -> TestRunner {
    let config = Config {
        cases,
        // Failures are reported in full; keep the working tree clean.
        failure_persistence: None,
        max_shrink_iters: 4096,
        ..Config::default()
    };
    TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &SEED))
}

/// Weighted so that requests and their deliveries dominate while owner
/// actions and time still interleave.
fn action_strategy() -> impl Strategy<Value = Action> {
    let alphabet = full_alphabet();
    let class = |keep: fn(&Action) -> bool| {
        let members: Vec<Action> = alphabet.iter().copied().filter(keep).collect();
        proptest::sample::select(members)
    };
    prop_oneof![
        4 => class(|a| matches!(a, Action::Request { .. } | Action::StaleKeyRequest(_))),
        4 => class(|a| matches!(a, Action::Deliver { .. })),
        2 => class(|a| matches!(a, Action::SetPolicy { .. } | Action::GrantMandate(..)
            | Action::RevokeMandate | Action::Freeze | Action::Unfreeze)),
        2 => class(|a| matches!(a, Action::ProposePolicy { .. } | Action::ProposeUnfreeze
            | Action::ExecutePending(_) | Action::CancelPending(_))),
        1 => class(|a| matches!(a, Action::Advance(_))),
    ]
}

/// Runs `cases` random sequences of up to `max_len` actions. Returns the kinds
/// of step that occurred, or the minimal failing sequence on a violation.
pub fn run(root: &World, cases: u32, max_len: usize) -> Result<BTreeSet<String>, String> {
    let coverage = RefCell::new(BTreeSet::new());
    let config = Config {
        cases,
        // Failures are reported in full below; keep the working tree clean.
        failure_persistence: None,
        max_shrink_iters: 4096,
        ..Config::default()
    };
    let mut runner =
        TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &SEED));
    let strategy = proptest::collection::vec(action_strategy(), 1..=max_len);

    let outcome = runner.run(&strategy, |actions| {
        let mut world = root.clone();
        let mut done = Vec::new();
        for action in actions {
            done.push(action);
            world = match guarded(|| world.apply(action)) {
                Ok(Ok((next, observed))) => {
                    coverage.borrow_mut().insert(observed.key());
                    next
                }
                Ok(Err(violation)) => {
                    return Err(TestCaseError::fail(report(&done, &violation.0)));
                }
                Err(panic) => {
                    return Err(TestCaseError::fail(report(
                        &done,
                        &format!("the harness panicked: {panic}"),
                    )));
                }
            };
        }
        Ok(())
    });

    match outcome {
        Ok(()) => Ok(coverage.into_inner()),
        Err(TestError::Fail(reason, minimal)) => Err(format!(
            "minimal failing sequence ({} actions): {minimal:?}\n{reason}",
            minimal.len()
        )),
        Err(TestError::Abort(reason)) => Err(format!("proptest aborted: {reason}")),
    }
}
