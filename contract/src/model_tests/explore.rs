//! Exhaustive driver: explicit-state search of every action sequence up to a
//! depth, over a reduced alphabet.
//!
//! It is a depth-first search that shares prefixes (each node applies one
//! action to a saved world) and deduplicates states: a world reached again
//! with no more remaining depth than before is not expanded again. So every
//! state reachable within the depth is visited, and every enabled action from
//! each is executed and checked. Skipping a repeated state is sound because
//! the future is a function of the state (the stored contract state, the
//! model, the clock and the in-flight queue are all in the fingerprint).
//! Actions that cannot change anything are skipped for the same reason.

use std::collections::{BTreeSet, HashMap};

use super::actions::Action;
use super::world::{guarded, World};

#[derive(Default, Debug)]
pub struct Stats {
    /// Distinct states expanded.
    pub states: usize,
    /// Actions executed against the real contract.
    pub transitions: usize,
    /// Transitions that led to an already explored state.
    pub deduplicated: usize,
    /// Actions skipped because they cannot change the world.
    pub skipped_noops: usize,
    /// The kinds of step that actually happened.
    pub coverage: BTreeSet<String>,
}

struct Search<'a> {
    alphabet: &'a [Action],
    visited: HashMap<u64, usize>,
    stats: Stats,
}

/// Explores all sequences of at most `depth` actions from `root`. On a
/// violation returns the offending sequence.
pub fn explore(root: &World, alphabet: &[Action], depth: usize) -> Result<Stats, String> {
    let mut search = Search {
        alphabet,
        visited: HashMap::new(),
        stats: Stats::default(),
    };
    search.visited.insert(root.fingerprint(), depth);
    search.stats.states += 1;
    visit(root, depth, &mut Vec::new(), &mut search)?;
    Ok(search.stats)
}

fn visit(
    world: &World,
    remaining: usize,
    path: &mut Vec<Action>,
    search: &mut Search,
) -> Result<(), String> {
    if remaining == 0 {
        return Ok(());
    }
    for &action in search.alphabet {
        if world.model.is_noop(action) {
            search.stats.skipped_noops += 1;
            continue;
        }
        path.push(action);
        let (next, observed) = match guarded(|| world.apply(action)) {
            Ok(Ok(step)) => step,
            Ok(Err(violation)) => return Err(report(path, &violation.0)),
            Err(panic) => return Err(report(path, &format!("the harness panicked: {panic}"))),
        };
        search.stats.transitions += 1;
        search.stats.coverage.insert(observed.key());

        let fingerprint = next.fingerprint();
        let left = remaining - 1;
        match search.visited.get(&fingerprint) {
            Some(&explored) if explored >= left => search.stats.deduplicated += 1,
            _ => {
                if search.visited.insert(fingerprint, left).is_none() {
                    search.stats.states += 1;
                }
                visit(&next, left, path, search)?;
            }
        }
        path.pop();
    }
    Ok(())
}

pub fn report(path: &[Action], reason: &str) -> String {
    let steps: Vec<String> = path
        .iter()
        .enumerate()
        .map(|(i, action)| format!("  {}. {action:?}", i + 1))
        .collect();
    format!("{reason}\nafter this sequence:\n{}", steps.join("\n"))
}
