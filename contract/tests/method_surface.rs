//! Snapshot of the contract's public method surface.
//!
//! Every exported method is a way to reach the contract, and a method's kind
//! (view or call) and modifiers (`init`, `payable`, `private`) decide who may
//! use it and with what: they are the role-separation facts. This test builds
//! the production contract (no test features) with `cargo near`, takes the
//! methods the wasm really exports, reads each one's kind and modifiers from
//! the ABI cargo-near emits, and compares `kind name [modifiers]` against the
//! committed `tests/snapshots/method-surface.txt`. Any new, removed or renamed
//! method, or a change of kind or payability, fails it.
//!
//! The export section is the authority for *which* methods exist, so a symbol
//! exported by hand (outside `#[near]`, hence missing from the ABI) is caught,
//! and the ABI is not trusted to list only real methods: near-sdk emits ABI
//! entries for `#[cfg(feature = ...)]` methods that are compiled out, see
//! `ABI_ONLY`.
//!
//! Update the snapshot only deliberately, after reviewing the change:
//!
//!   MINDER_UPDATE_SURFACE_SNAPSHOT=1 cargo test --test method_surface
//!
//! Deterministic and offline: the ABI's build metadata (compiler, wasm hash) is
//! ignored, and methods are sorted.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use near_sdk::serde_json::Value;

const SNAPSHOT: &str = "tests/snapshots/method-surface.txt";
const UPDATE_VARIABLE: &str = "MINDER_UPDATE_SURFACE_SNAPSHOT";

/// Exports every near-sdk wasm has that are not contract methods. (The
/// NEP-330 view `contract_source_metadata` is a method, and in the ABI.)
const RUNTIME_EXPORTS: [&str; 4] = ["memory", "__data_end", "__heap_base", "__contract_abi"];

/// Methods the ABI lists although the production wasm does not export them:
/// near-sdk's `#[near]` macro emits an ABI entry for a method even when a
/// `#[cfg(feature = "test-helpers")]` compiles it out. It is a documentation
/// artifact, not a reachable method, so it is not part of the surface, but it
/// is named here so any *other* ABI/export disagreement still fails.
const ABI_ONLY: [&str; 1] = ["test_record_spend"];

const SNAPSHOT_HEADER: &str = "\
# Minder public method surface: one line per exported contract method, as
# `kind name [modifiers]`, sorted by name.
# Source: the cargo-near ABI of the production build (no test features),
# cross-checked against the wasm export section.
# Generated; do not edit by hand. After reviewing a change to the surface,
# regenerate it deliberately:
#   MINDER_UPDATE_SURFACE_SNAPSHOT=1 cargo test --test method_surface
";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Method {
    kind: String,
    modifiers: Vec<String>,
}

impl Method {
    fn describe(&self) -> String {
        if self.modifiers.is_empty() {
            self.kind.clone()
        } else {
            format!("{} [{}]", self.kind, self.modifiers.join(", "))
        }
    }
}

type Surface = BTreeMap<String, Method>;

// ---- Building the production contract and reading it ----

struct Build {
    abi: Value,
    wasm: Vec<u8>,
}

/// Builds the production contract once per test binary, into its own target
/// directory so it never disturbs `target/near`.
fn build() -> &'static Build {
    static BUILD: OnceLock<Build> = OnceLock::new();
    BUILD.get_or_init(|| {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let out_dir = format!("{manifest_dir}/target/surface-wasm");
        let output = Command::new("cargo")
            .args(["near", "build", "non-reproducible-wasm", "--locked"])
            .args(["--out-dir", &out_dir])
            .env(
                "CARGO_TARGET_DIR",
                format!("{manifest_dir}/target/surface-build"),
            )
            .current_dir(manifest_dir)
            .output()
            .expect("cargo near must be installed");
        assert!(
            output.status.success(),
            "building the production contract failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let abi = fs::read(format!("{out_dir}/minder_abi.json")).expect("ABI file");
        Build {
            abi: near_sdk::serde_json::from_slice(&abi).expect("ABI is JSON"),
            wasm: fs::read(format!("{out_dir}/minder.wasm")).expect("wasm file"),
        }
    })
}

/// Every function the ABI lists, with its kind and modifiers.
fn abi_methods(abi: &Value) -> Surface {
    abi["body"]["functions"]
        .as_array()
        .expect("ABI functions")
        .iter()
        .map(|function| {
            let mut modifiers: Vec<String> = function["modifiers"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .map(|m| m.as_str().expect("modifier").to_string())
                        .collect()
                })
                .unwrap_or_default();
            modifiers.sort();
            (
                function["name"].as_str().expect("name").to_string(),
                Method {
                    kind: function["kind"].as_str().expect("kind").to_string(),
                    modifiers,
                },
            )
        })
        .collect()
}

/// The names in the wasm export section.
fn wasm_exports(wasm: &[u8]) -> BTreeSet<String> {
    fn leb(bytes: &[u8], at: &mut usize) -> usize {
        let (mut value, mut shift) = (0usize, 0);
        loop {
            let byte = bytes[*at];
            *at += 1;
            value |= usize::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                return value;
            }
        }
    }
    let mut at = 8; // magic number and version
    while at < wasm.len() {
        let section = wasm[at];
        at += 1;
        let size = leb(wasm, &mut at);
        if section == 7 {
            let mut cursor = at;
            let count = leb(wasm, &mut cursor);
            return (0..count)
                .map(|_| {
                    let length = leb(wasm, &mut cursor);
                    let name = String::from_utf8(wasm[cursor..cursor + length].to_vec())
                        .expect("export name");
                    cursor += length + 1; // the kind byte
                    leb(wasm, &mut cursor); // the index
                    name
                })
                .collect();
        }
        at += size;
    }
    panic!("the wasm has no export section");
}

/// The production surface: the methods the wasm exports, as the ABI describes
/// them.
fn exported_surface(build: &Build) -> Surface {
    let exports = wasm_exports(&build.wasm);
    abi_methods(&build.abi)
        .into_iter()
        .filter(|(name, _)| exports.contains(name))
        .collect()
}

// ---- The snapshot file ----

fn render(surface: &Surface) -> String {
    let mut text = SNAPSHOT_HEADER.to_string();
    for (name, method) in surface {
        let line = if method.modifiers.is_empty() {
            format!("{} {name}", method.kind)
        } else {
            format!("{} {name} [{}]", method.kind, method.modifiers.join(", "))
        };
        text.push_str(&line);
        text.push('\n');
    }
    text
}

fn parse(text: &str) -> Surface {
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (kind, rest) = line.split_once(' ').expect("`kind name [modifiers]`");
            let (name, modifiers) = match rest.split_once(" [") {
                Some((name, modifiers)) => (
                    name,
                    modifiers
                        .trim_end_matches(']')
                        .split(", ")
                        .map(str::to_string)
                        .collect(),
                ),
                None => (rest, Vec::new()),
            };
            (
                name.to_string(),
                Method {
                    kind: kind.to_string(),
                    modifiers,
                },
            )
        })
        .collect()
}

/// What differs between the committed surface and the built one, or `None`.
fn describe_changes(committed: &Surface, built: &Surface) -> Option<String> {
    let mut lines = Vec::new();
    for (name, method) in built {
        match committed.get(name) {
            None => lines.push(format!("  + added:   {name} ({})", method.describe())),
            Some(old) if old != method => lines.push(format!(
                "  ~ changed: {name}: {} -> {}",
                old.describe(),
                method.describe()
            )),
            Some(_) => {}
        }
    }
    for (name, method) in committed {
        if !built.contains_key(name) {
            lines.push(format!("  - removed: {name} ({})", method.describe()));
        }
    }
    (!lines.is_empty()).then(|| {
        format!(
            "The contract's public method surface changed:\n{}\n\n\
             Every method is a way in, and kind and payability decide who can use it, so \
             review this against the role separation in DESIGN.md. If the change is \
             intended, the snapshot has to be updated deliberately:\n  \
             {UPDATE_VARIABLE}=1 cargo test --test method_surface\n\
             and the diff of {SNAPSHOT} reviewed and committed with the change.",
            lines.join("\n")
        )
    })
}

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT)
}

// ---- The tests ----

#[test]
fn public_method_surface_matches_the_committed_snapshot() {
    let built = exported_surface(build());

    if std::env::var_os(UPDATE_VARIABLE).is_some() {
        fs::create_dir_all(snapshot_path().parent().expect("directory")).expect("create directory");
        fs::write(snapshot_path(), render(&built)).expect("write the snapshot");
        println!("updated {SNAPSHOT} with {} methods", built.len());
        return;
    }

    let committed = parse(&fs::read_to_string(snapshot_path()).unwrap_or_else(|_| {
        panic!("{SNAPSHOT} is missing; create it with {UPDATE_VARIABLE}=1 cargo test --test method_surface")
    }));
    if let Some(changes) = describe_changes(&committed, &built) {
        panic!("{changes}");
    }
    assert!(!built.is_empty(), "an empty surface cannot be the snapshot");
}

#[test]
fn exports_and_abi_agree_apart_from_the_named_exception() {
    // An export missing from the ABI would be callable but invisible to the
    // snapshot (for instance a hand-written `#[no_mangle] extern "C"`
    // function), and an ABI entry with no export would make the snapshot lie.
    let build = build();
    let abi: BTreeSet<String> = abi_methods(&build.abi).into_keys().collect();
    let exports = wasm_exports(&build.wasm);
    let runtime: BTreeSet<String> = RUNTIME_EXPORTS.iter().map(|s| s.to_string()).collect();
    let exported_methods: BTreeSet<String> = exports.difference(&runtime).cloned().collect();
    let abi_only: BTreeSet<String> = ABI_ONLY.iter().map(|s| s.to_string()).collect();

    let unlisted: Vec<&String> = exported_methods.difference(&abi).collect();
    let phantom: Vec<&String> = abi
        .difference(&exported_methods)
        .filter(|name| !abi_only.contains(*name))
        .collect();
    assert!(
        unlisted.is_empty() && phantom.is_empty(),
        "the wasm exports and the ABI disagree. Exported but not in the ABI: {unlisted:?}. \
         In the ABI but not exported (and not the known exception): {phantom:?}."
    );
    // The exception must still be real, or it would silently excuse a future
    // phantom method of that name.
    for name in &ABI_ONLY {
        assert!(
            abi.contains(*name) && !exports.contains(*name),
            "{name} is listed in ABI_ONLY but is no longer an ABI-only method"
        );
    }
}

#[test]
fn the_snapshot_text_round_trips() {
    let text = fs::read_to_string(snapshot_path()).expect("the committed snapshot");
    assert_eq!(
        render(&parse(&text)),
        text,
        "the snapshot is not in canonical form"
    );
}

#[test]
fn changes_to_the_surface_are_named_in_the_failure_message() {
    let committed = parse("call freeze\ncall unfreeze\nview get_config\ncall deposit [payable]\n");
    let built = parse(
        "call freeze [payable]\nview get_config\ncall deposit [payable]\ncall zz_new\n\
         call unfreeze_renamed\n",
    );
    let message = describe_changes(&committed, &built).expect("a change");
    assert!(message.contains("+ added:   zz_new"));
    assert!(message.contains("+ added:   unfreeze_renamed"));
    assert!(message.contains("- removed: unfreeze"));
    assert!(message.contains("~ changed: freeze: call -> call [payable]"));
    assert!(message.contains("updated deliberately"));
    assert!(message.contains(UPDATE_VARIABLE));
    assert!(describe_changes(&committed, &committed).is_none());
}
