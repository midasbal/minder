//! Kani proofs of the contract's pure modules, compiled only under
//! `cargo kani` (see DESIGN.md, "Formal verification").
//!
//! The modules are the contract's own source files, included by path, not
//! copies, so a proof is always about the code that ships. This crate is not
//! part of the contract's build: the contract's wasm, its tests and its
//! manifest do not see it.

#[path = "../../src/types.rs"]
pub mod types;

#[path = "../../src/loosening.rs"]
pub mod loosening;

#[path = "../../src/policy.rs"]
pub mod policy;

#[path = "../../src/window.rs"]
pub mod window;

#[path = "../../src/evm.rs"]
pub mod evm;

#[cfg(kani)]
mod loosening_proofs;
#[cfg(kani)]
mod proofs;
