# Minder: security self-audit

|  |  |
| --- | --- |
| **Subject** | The Minder contract, `contract/` in this repository |
| **Deployed instance** | `gate.minder-gate-owner-bf5fcd.testnet` (NEAR testnet), wasm sha256 `2fdd044411312fba8bc18b91ce06138b0c933cdb32bbb5296964f6b35ba54d09` |
| **Reviewed at** | commit `dfe065c`, 2026-11-14 (the source at this commit builds to the deployed wasm, Section 6) |
| **Reviewer** | The author. **This is a self-audit, not an independent audit.** No third party has reviewed this code. |
| **Status** | Testnet only. Never deployed to mainnet. |

This document is a security review written to be checked. Every claim is sourced from `contract/DESIGN.md`, the
code, or a result that can be re-run, and each one names the test or proof that backs it. Where something is not
proved, not tested, or not known, it says so. Section 5 lists the things this design does not protect against.
Every result is bounded by the limits in Section 5 (L9 for the proofs), not absolute.

## Contents

1. [Scope and methodology](#1-scope-and-methodology)
2. [Trust model](#2-trust-model)
3. [Threat, mechanism and evidence](#3-threat-mechanism-and-evidence)
4. [Findings from our own tooling](#4-findings-from-our-own-tooling)
5. [Known limitations and accepted risks](#5-known-limitations-and-accepted-risks)
6. [Verification summary and proof ledger](#6-verification-summary-and-proof-ledger)
7. [Reproducing this review](#7-reproducing-this-review)

## 1. Scope and methodology

### In scope

| Component | Where | What it does |
| --- | --- | --- |
| The firewall decision | `src/policy.rs` (`policy::evaluate`) | The one pure function every request passes through: status, mandate, key, asset policy, recipient allowlist, per-transaction cap, window, budget, then the mandate's scope |
| The timelock and its classifier | `src/loosening.rs` (`loosens`), `src/timelock.rs` | Decides whether an owner change is instant (tightening) or waits the delay (loosening); proposal, execution, cancellation; the guardian role |
| The sliding window | `src/window.rs` (`SpendRing`) | Committed spend per asset over a window of `k + 1` buckets |
| The EVM signing gate and encoder | `src/gate.rs`, `src/evm.rs` | Builds the EIP-1559 transaction itself, asks the Chain Signatures contract to sign it, checks the signature recovers to the contract's own address, emits it |
| The mandate model | `src/lib.rs`, `src/types.rs` | The agent's function-call key, its scope, revocation and expiry, the reserve/settle/release accounting, owner custody methods |
| The keyless deployment | `contract/deploy/deploy-keyless.sh`, `tests/deploy.rs` | One batch with no `AddKey`, so the account never holds a full-access key |

### Out of scope, and trusted

- **The frontend and relay** (`web/`). They hold no key and sign nothing; the console only reads the chain and
  calls free view methods. They were not reviewed as part of this audit.
- **The NEAR runtime.** Enforcement of function-call access keys (method list, receiver, no deposit, no transfer,
  no key changes) is the runtime's, not the contract's. The contract relies on it (Section 2).
- **NEAR Chain Signatures and its MPC network.** The contract trusts the signer to sign only what it is asked,
  to derive the signing key from the calling account and path, and to answer. It does not trust the answer
  blindly (it checks the signature), but it cannot make the signer available or honest.
- **The destination EVM chain** and whoever broadcasts the emitted transaction.
- **`near-sdk` 5.29.1** and the Rust toolchain, beyond what the proofs state about the shipped source.

### Methods

| Method | What it covers | Result (Section 6) |
| --- | --- | --- |
| Manual review | The author's own read of every module against the stated invariants, recorded in the "Choices that need your review" and "Decision log" sections of `DESIGN.md`. Not independent. | n/a |
| Kani bounded model checking (0.68.0) | The pure logic: `evaluate`, the timelock classifier, the sliding window, EVM signature normalization. Holds for every input within stated bounds. | 43 harnesses |
| Mutation testing (`cargo-mutants` 27.1.0) | Whether the unit tests notice a change to the pure logic | 241 of 247 viable mutants killed, 6 documented equivalents |
| Differential testing against viem 2.57.16 | The hand-written EIP-1559 encoder, byte for byte | 1,743 cases |
| Stateful model-based harness (`proptest`) | The contract against an independently written shadow model, over every state reachable within depth 8, plus 4,000 random long sequences | 409,382 states |
| Byte-level fuzz tests | Signature parsing and assembly never panic and accept exactly the acceptable inputs | 10,000 cases per property |
| `near-workspaces` sandbox tests | The production wasm, with the real function-call key, against a real NEAR sandbox | see Section 6 |
| A live testnet run | The deployed contract, a real Chain Signatures signing and real Sepolia settlement | `web/client/src/data/live-run.json` |

## 2. Trust model

### The adversary

The **agent and its key are assumed fully compromised**: its code, its prompts, its tools, and the private key of
its function-call access key. The agent is also assumed able to read every receipt and event (they are public),
call any method its key can reach as often as its gas allowance allows, and choose any arguments.

### Who is trusted with what

| Party | Trusted to | Not trusted to |
| --- | --- | --- |
| **The contract code** | Be the only path from the agent to funds, and apply one policy to every request | (This is what the audit examines.) |
| **The NEAR runtime** | Refuse any call from the agent's key that is not one of the three request methods on the contract itself, with no attached deposit, and refuse transfers, deployments and key changes (`RequiresFullAccess`) | |
| **Chain Signatures** | Sign exactly the hash it is given with the key derived for this contract account and path; derive keys per calling account | The contract does not assume the response is valid: it parses, normalizes and `ecrecover`s it |
| **The owner** | Be the **permanent root**: no `set_owner`, so whoever signs the keyless deployment is the owner for good. Owner recovery (`owner_withdraw` and the EVM forms) is deliberately outside policy | A single owner key is *not* assumed safe: loosening is delayed (Section 3, T11 and T12). The recommended mitigation is a multisig owner, which is external and not built here |
| **The guardian** | Tighten only: `freeze`, `revoke_mandate`, `cancel_change` (not a change about itself) | Anything that moves or widens authority. A compromised guardian can only deny service |
| **The agent** | Nothing | Everything |

### What the contract guarantees, and what it relies on

The contract itself guarantees (and the evidence for each is in Section 3):

- every agent request is decided by `policy::evaluate` and denied unless every limit holds;
- spend is reserved before any promise exists and booked or released exactly once;
- the contract builds every EVM transaction itself (the agent supplies only `to` and `value`, or `token`,
  `recipient` and `amount`) and emits only a transaction whose signature recovers to its own address;
- no change that loosens authority takes effect before the timelock delay, and tightening is never blocked.

It relies externally on:

- the runtime enforcing the key's restrictions, and the account having **no full-access key** (checked on chain,
  `scripts/verify-deployment.sh`);
- Chain Signatures deriving keys per calling account, so the contract's EVM address is one only this contract can
  sign for, and being available (a signer outage fails closed: nothing is signed, nothing is booked);
- the owner setting a correct EVM `sender` and sensible gas limits (a wrong `sender` fails closed; a wrong gas limit
  wastes budget, Section 5, L6);
- someone watching and holding the means to cancel inside the timelock delay (Section 5, L2).

## 3. Threat, mechanism and evidence

How to read the "Verified by" column. Every name was checked against the source (it exists as a function in
`contract/`), but the test names are as `DESIGN.md` gives them. "Unit" names with a module prefix (`policy::`,
`window::`, `gate_tests::`, `budget_tests::` and so on) are tests in `contract/src`. "Sandbox" names are tests in
`contract/tests` that run the production wasm in a NEAR sandbox: those with a prefix (`agent_key::`, `evm_gate::`,
`timelock::`, `transfers::`, `deploy::`) are in the file of that name, and those without one are in
`tests/invariants.rs`. "Kani" names are proof harnesses in `contract/kani/src`.

| # | Threat | Mechanism | Verified by | Residual |
| --- | --- | --- | --- | --- |
| T1 | **Exceed the per-transaction cap** | `evaluate` denies `PerTxCapExceeded` when `amount > cap`; the mandate's own cap applies after, the smaller of the two binds. For EVM the amount counted is the outlay (value plus worst-case fee) | Sandbox `amount_over_per_tx_cap_is_denied` (`tests/invariants.rs`); unit `policy::caps_are_inclusive`, `policy::the_effective_cap_is_the_smaller_of_the_global_and_the_mandates`. Kani `allow_implies_every_limit_holds` (an Allow implies `amount <= per_tx_cap`, with and without a recipient scope) | The Kani bounds on key, asset and scope size (Section 6) |
| T2 | **Drain through many small payments** | Three independent limits: the per-transaction cap; the **sliding window** (`effective_spend + amount <= window_limit`, where effective spend = committed + in-flight reserved); the **epoch budget**, a per-asset aggregate for the budget period that outlasts windows and re-grants | Sandbox `cumulative_spend_within_one_window_is_capped`; unit `window::spend_is_never_forgotten_early_and_at_most_one_slot_late`, `budget_tests::the_budget_denies_when_committed_spend_plus_the_request_would_exceed_it`, `budget_tests::the_budget_binds_before_the_window_when_it_is_the_smaller`. Kani `the_ring_never_under_counts_with_1_slot` (and 2, 3). The model harness checks, from the record of bookings alone, that spend inside any real window-length interval is at most `window_limit` and per budget period at most `min(windows * limit, budget)` | The window over-enforces by up to one slot width (errs towards denying). Worst-case loss per asset is `min(sum of the windows over the period, epoch_budget)` (`DESIGN.md`, "Lifetime budget") |
| T3 | **Pay an unlisted recipient** | `RecipientNotAllowed` against the owner's allowlist (NEAR and EVM lists are separate); a scoped mandate can narrow further (`RecipientOutOfScope`). A token payment's recipient is decoded from the request and checked on the EVM list | Sandbox `recipient_not_on_allowlist_is_denied`; unit `gate_tests::the_evm_allowlist_is_separate_from_the_near_allowlist`, `gate_tests::the_decoded_recipient_must_be_on_the_evm_allowlist`. Kani `allow_implies_every_limit_holds` (an Allow implies the recipient is allowlisted) | The EVM recipient *scope* is unit-tested but not Kani-proved (Section 5, L9) |
| T4 | **Call a method outside the three, or attempt AddKey, Transfer or DeployContract** | The agent is a native function-call key on the contract's own account: receiver the contract, `method_names` fixed in code (`AGENT_METHODS`), no deposit. The NEAR runtime refuses everything else before any contract code runs; the account has no full-access key | Sandbox `agent_key::the_protocol_refuses_the_agent_key_every_method_but_the_two_agent_ones` (every exported method except the three request methods, over 25 of them, each refused as `MethodNameMismatch`, with no contract logic run; the test's name predates the third method), `agent_key::the_agent_key_cannot_call_another_contract_such_as_the_signer`, `agent_key::the_agent_key_cannot_do_anything_but_call_the_two_methods` (transfers, key changes and deployment are refused as `RequiresFullAccess`); `deploy::*` and `scripts/verify-deployment.sh` for the zero full-access keys | **Relies on the runtime** (Section 2). The key can burn its 0.25 NEAR gas allowance, including on denied calls (L8) |
| T5 | **The agent widens its own authority** | Policy and mandate changes are owner-only and unreachable by the agent's key. A mandate's scope can only subtract: effective authority = policy ∩ scope ⊆ policy. A re-grant, even to the agent's own key, can never exceed the stored policy | Sandbox `owner_only_methods_revert_for_everyone_else`; `agent_key::the_protocol_refuses_the_agent_key_every_method_but_the_two_agent_ones`. Kani `a_scope_never_widens_authority` (and `_with_a_recipient_scope`), `allow_implies_every_limit_holds`, `the_deny_reason_is_the_first_failing_check_in_the_documented_order` | Instant scope widening by re-grant is deliberate (L4) |
| T6 | **ERC-20 `approve`, unlimited approval, calldata abuse** | The contract builds the calldata itself: only `transfer(address,uint256)`, selector `0xa9059cbb`, 68 bytes; the agent supplies `(token, recipient, amount)` and never raw calldata. There is no code path that builds `approve`, `transferFrom` or a call to an arbitrary contract. The amount is a `u128`. The token must itself have an owner-set policy | Unit `gate_tests::the_firewall_builds_exactly_the_transfer_calldata_and_hashes_it`, `gate_tests::the_calldata_matches_viems_abi_encoding`, `evm::unsigned_encoding_matches_viem` (its vectors include `erc20_transfer` and `erc20_transfer_max_amount`); the corpus test `evm_corpus::encoder_matches_viem_byte_for_byte_over_the_whole_corpus` (340 token cases rebuild the calldata and compare with viem); `signing_confinement::signing_dispatch_is_confined_to_the_allowed_functions`; unit `gate_tests::a_token_without_a_policy_is_denied` | Amounts are `u128`, narrower than `uint256` (a limit, not a risk). The contract cannot see the outcome on the destination chain (L5) |
| T7 | **Drain through fees or the gas leg** | Policy counts the **outlay**, `value + gas_limit * max_fee_per_gas`, for the cap, window, reservation and booking. A token payment has two legs: the token amount against the token's policy and its worst-case gas against the native asset's policy (cap, window, budget), reserved together before any promise | Unit `gate_tests::fees_count_toward_the_per_transaction_cap`, `gate_tests::many_tiny_payments_cannot_drain_through_fees`, `gate_tests::gas_that_would_exceed_the_native_policy_is_denied_on_the_gas_leg`, `gate_tests::a_token_transfer_with_no_native_policy_is_denied_clearly`, `gate_tests::both_reservations_are_made_together_and_the_second_request_sees_them`; the model harness `token` suite (19,178 states) | The worst-case fee over-counts (errs towards denying). Fees and gas limit are owner-set; the agent supplies neither |
| T8 | **Signature malleability, high-`s`, invalid signer output** | `normalize_s` flips a high `s` to `n - s` and the parity with it; `try_assemble_signed` checks recovery id 0 or 1, `0 < r, s < n`, the point's shape, and never panics; `env::ecrecover` is called with the malleability flag set | Kani `normalize_s_is_total_and_always_low_s` (all 2^256 values of `s`), `normalized_signature_is_total_and_yields_only_valid_signatures` (point and scalar lengths 0 to 34 bytes; longer inputs take the same early return). Unit `evm::high_s_is_flipped_and_parity_toggled`, `evm::low_s_passes_through_unchanged`, `gate_tests::a_high_s_signature_is_normalized_before_the_transaction_is_emitted`; fuzz `gate::tests::try_assemble_signed_never_panics_and_accepts_exactly_the_acceptable_signatures` | Panic-freedom of `encode_signed` over every `r`, `s` and numeric field is **not** proved (L9) |
| T9 | **A forged or wrong-key signature emitted; blind signing of an attacker-chosen hash** | The contract computes the signing hash itself from owner config, its own nonce and the validated `to` and `value`; the callback carries the same transaction by value; the emitted transaction must `ecrecover` to the configured `sender`. No method signs a caller-supplied hash; only four functions may dispatch `sign` (the two agent payment methods and the two owner recovery paths), and a source-scanning test fails if any other does | Unit `gate_tests::the_contract_sends_the_signer_a_hash_it_computed_itself`, `gate_tests::a_signature_by_another_key_is_booked_but_not_emitted`, `gate_tests::a_signature_over_a_different_transaction_is_booked_but_not_emitted`, `gate_tests::a_sender_that_differs_from_the_signing_key_blocks_emission`, `evm::a_real_mpc_signature_recovers_to_the_derived_address` (ground truth from `v1.signer-prod.testnet`); sandbox `evm_gate::the_signer_is_sent_the_hash_the_contract_computed`; `signing_confinement::signing_dispatch_is_confined_to_the_allowed_functions` (`tests/signing_confinement.rs`) and `tests/method_surface.rs` (the public surface is a committed snapshot) | A wrong `sender` configuration fails closed: nothing is emitted, the budget is consumed |
| T10 | **TOCTOU: concurrent requests passing the same budget** | The **reserve-then-settle** model: an allowed request adds its amount to `reserved[asset]` *before any promise exists*, so a request in a later receipt already counts it. Effective spend (window) and budget usage both include the reservation. The callback releases it exactly once and books it on success; callbacks never panic | Unit `lifecycle_tests::reservation_is_visible_before_the_callback_runs`, `lifecycle_tests::a_call_made_while_another_is_in_flight_cannot_exceed_the_limit`, `budget_tests::in_flight_requests_count_against_the_budget_before_they_settle`, `gate_tests::a_second_request_sees_the_first_ones_reservation`; sandbox `transfers::reservation_is_visible_and_binding_while_the_transfer_is_in_flight`, `evm_gate::a_second_request_sees_the_first_ones_reservation_before_it_settles`; the model harness (in-flight queue, deliveries in either order) | The reservation logic in `lib.rs` is **not** Kani-proved; it is covered by the model harness and tests (L9) |
| T11 | **A malicious owner loosens policy** | Every owner change is classified by `loosens`: tightening and emergency actions are instant, loosening ones are refused when called directly (`Timelocked`) and must be proposed, wait `timelock_delay`, then be executed. A guardian or the owner can cancel in between | Kani soundness for every `OwnerChange` variant (no loosening is ever classified instant), e.g. `a_policy_that_admits_more_is_never_instant`, `adding_a_near_recipient_is_never_instant`, `raising_an_evm_bound_or_moving_the_chain_is_never_instant`, `removing_or_replacing_the_guardian_is_never_instant`; completeness per variant (12 harnesses); unit `timelock_tests::a_raised_cap_window_or_budget_is_delayed`, `timelock_tests::adding_a_recipient_is_delayed_on_both_surfaces`, `timelock_tests::a_mixed_tightening_and_loosening_policy_change_is_loosening`; mutation tests on the classifier | Soundness holds for the stored-state *questions* the classifier asks; the lookups in the `Minder` implementation are covered by tests and mutation, not proved (Section 6) |
| T12 | **A compromised owner key** | It cannot widen authority or redirect funds at once: loosening waits the delay; `owner_withdraw` is instant only to a pre-registered treasury (changing it is itself timelocked); a grant to its own key is bounded by the un-loosened policy. The guardian can `freeze`, `revoke_mandate` and `cancel_change` instantly | Unit `timelock_tests::a_compromised_owner_key_cannot_widen_authority_or_redirect_funds_at_once`, `timelock_tests::tightening_works_while_a_loosening_of_the_same_thing_is_pending`; sandbox `timelock::a_compromised_owner_cannot_loosen_or_redirect_at_once_but_can_still_freeze`; unit `guardian_tests::a_standoff_between_owner_and_guardian_cannot_lose_funds`; the model harness `timelock` (38,605 states) and `guardian` (65,036 states) suites | **Not safe against a single owner key alone.** The delay helps only if someone else is watching and can cancel (L2). An owner and a guardian acting together can drain after the delay |
| T13 | **Refreshing a spent budget by re-granting the mandate** | The budget belongs to a **budget period**, independent of the key epoch. A routine re-grant or rotation carries usage forward; only a deliberate `reset_budget` starts a new period, and when spend exists that reset is a timelocked loosening | Unit `budget_tests::a_regrant_carries_the_budget_forward_and_the_key_epoch_moves_alone`, `budget_tests::a_compromised_owner_cannot_reset_a_used_budget_at_once`, `budget_tests::removing_and_re_adding_a_policy_does_not_reset_the_budget`; Kani `extending_a_live_mandate_or_refunding_budget_is_never_instant`; the model harness `budget` (98,543 states) and `budget reset` (73,949 states) suites | This was a real flaw, fixed (Section 4, F1) |
| T14 | **A compromised guardian** | The guardian's powers are exactly the three stop-powers. It cannot grant, change policy, add a recipient, reconfigure, change a treasury, `unfreeze` or withdraw; it cannot cancel a change to itself | Unit `guardian_tests::every_other_owner_method_is_refused_for_the_guardian`, `guardian_tests::a_compromised_guardian_can_only_stop_never_move_or_widen`, `guardian_tests::the_guardian_cannot_cancel_its_own_removal_or_replacement_but_the_owner_can`; Kani `removing_or_replacing_the_guardian_is_never_instant` | A hostile guardian can keep the contract frozen until the owner removes it, which costs the owner two delays; funds are never at stake |
| T15 | **Replaying or reusing an EVM nonce** | The contract owns one nonce counter per chain id, taken synchronously at request time; the agent supplies neither nonce nor gas. The counter only rises | Unit `gate_tests::each_request_gets_its_own_nonce_and_hash`, `gate_tests::token_payments_share_the_nonce_counter_with_native_payments`; the model harness: no `(chain, nonce)` pair is ever issued twice, a failed signing still consumes its nonce | A signing that fails leaves a **nonce gap** (L5) |
| T16 | **A forged settlement callback** (to release or book spend) | Callbacks are `#[private]`: the predecessor must be the contract account. The account holds no full-access key, so no outside party can be that predecessor; the agent's key reaches only the three request methods | Sandbox `transfers::only_the_contract_itself_can_call_the_settlement_callback`; `deploy::*` | |
| T17 | **Acting as the contract, redeploying it, or changing its code** | The account is created keyless: one batch of `CreateAccount`, `Transfer`, `DeployContract`, `FunctionCall(init)`, no `AddKey`. There is no key to sign with, and no upgrade path | Sandbox `deploy::*` (six tests on the production wasm, including a negative control with an `AddKey` that the same check catches); `scripts/verify-deployment.sh` (live chain, Section 6) | Keyless means **no upgrade path** (L10) |
| T18 | **Using a revoked, expired or stale key** | `evaluate` checks the `revoked` flag *before* the key (synchronous, whether or not the key deletion has landed) and expiry (`now >= expiry`); the key is also deleted at the protocol level | Unit `mandate_tests::a_request_after_revoking_is_denied_by_the_flag_with_no_deletion_landed`, `mandate_tests::an_expired_mandate_denies_even_for_its_own_key`; sandbox `agent_key::after_a_revoke_the_epoch_guard_denies_even_if_the_key_survived_the_delete`. Kani `frozen_revoked_expired_and_wrong_key_always_deny` | An expired key stays on the account, harmless, until the next revoke or grant |
| T19 | **A panic that strands a reservation** (denying service by leaving `reserved` inflated) | The settlement callbacks never panic: they release unconditionally and saturate; signer output is read with `promise_result_checked` and parsed by the contract, never `#[callback_result]` | Unit `lifecycle_tests::settlement_never_panics_even_if_it_releases_more_than_is_reserved`, `gate_tests::settlement_never_panics_even_if_it_releases_more_than_is_reserved`, `gate::tests::malformed_responses_are_rejected_without_panicking`; fuzz `gate::tests::assemble_from_response_never_panics_on_arbitrary_bytes` | |

## 4. Findings from our own tooling

This section separates three kinds of result, because they are not the same: **defects that existed and were
fixed** (F1, F2), **test gaps the tooling exposed** (F3), **faults deliberately injected to prove the tests would
notice** (F4), and **operational hazards found in live use** (F5). Nothing here was found by an independent
party.

### F1. Budget refresh by re-granting (a real flaw, fixed)

**What was wrong.** The lifetime budget was keyed by the mandate's key epoch, and every grant bumps that epoch.
So every grant, including an instant `revoke_mandate` followed by `grant_mandate`, handed back a spent budget: "a
compromised owner key could refresh it at will" (`DESIGN.md`, "Lifetime budget per budget period"). The budget
did not bound a period, only a grant.

**The fix** (commit `daab164`, 2026-11-13). Budget counters are keyed by a contract-wide **budget epoch**,
independent of the key epoch. A re-grant or key rotation now carries usage forward. A fresh period needs an
explicit `reset_budget`; with no spend it is an instant no-op, and with spend it is a classified, timelocked
loosening.

**How it is held.** The unit tests and Kani proof in T13; the model harness gained the `budget` and
`budget reset` suites, which explore 98,543 and 73,949 states and check that a delivery landing after a reset is
booked to the new period and that the reset is refused when called and held when proposed.

**How it was found.** The record does not say which tool surfaced it; it is described in `DESIGN.md` as the
pre-fix behaviour.

### F2. Under-counted EVM spend when the signer's answer was unusable (the "G1" rule, a real flaw, fixed)

**What was wrong.** An EVM payment booked its spend only after the contract had itself parsed and assembled the
signer's response. But the signer's response sits in a public receipt: a signature the contract fails to read
(a response-format change, or one past the 4,096-byte read buffer) is still a signature anyone can assemble and
broadcast. Spend that was signed but not booked is unaccounted, so the limits could be exceeded.

**The fix** (commit `67de7f1`, 2026-11-08). Booking keys on the receipt, never on the contract's ability to read
it: only a *failed* signer receipt books nothing; any successful receipt books every reserved amount, whether or
not a transaction is emitted. Emission separately depends on the authenticity check. The cost is deliberate
over-counting (a signature the contract refuses still uses budget for one window).

**How it is held.** Unit `gate_tests::a_successful_receipt_we_cannot_use_is_booked_but_emits_nothing` (eleven
cases: recovery id 2, zero `s`, `s` at the order, bad prefix, short point, not hex, wrong shape, not JSON, empty,
oversized garbage, oversized *valid* signature) and `gate_tests::only_a_failed_receipt_books_nothing`. The model
harness encodes the rule independently in its oracle. When the fix was temporarily reverted, the `accounting` and
`policy` suites failed, and the random driver shrank the failure to two actions (a mid-sized EVM request, then a
`WrongShape` delivery) (`DESIGN.md`, "What it caught when the fix was reverted").

### F3. Mutation testing: six real test gaps, six equivalents

`cargo-mutants` generated 259 mutants over the pure logic. 12 were viable and missed by the tests (12 more do not
compile and are unviable). Triage of the 12:

- **Six were genuine test gaps**, all in the timelock classifier `is_loosening`: no test sat on these boundaries.
  They are now pinned by four dedicated tests in `src/timelock_tests.rs`. Each was proved against its own
  mutant (the mutation applied by hand, the new test seen to fail, the source restored).

  | Mutant | Killed by |
  | --- | --- |
  | `SetEvmRecipient`: `&&` to `\|\|` | `an_evm_recipient_change_loosens_only_when_it_adds_a_new_address` |
  | `GrantMandate`: `<` to `<=` (a mandate is live strictly before its expiry) | `a_grant_extends_a_live_mandate_up_to_but_not_at_its_expiry_instant` |
  | `max_fee_per_gas`: `>` to `>=` | `each_evm_fee_and_gas_bound_loosens_only_when_strictly_raised` |
  | `max_priority_fee_per_gas`: `>` to `<` | the same |
  | `token_gas_limit`: `>` to `<` | the same |
  | `SetTimelockDelay`: `<` to `<=` | `the_timelock_delay_loosens_only_when_strictly_lowered` |

- **Six are documented equivalents** (cannot change observable behaviour): the two `+` operators in
  `ERC20_TRANSFER_CALLDATA_LEN` (three mutants on the constant's two `+` operators; it only sizes a
  `Vec::with_capacity`), `|` to `^` in `hex_decode` (the operands share no set bits), `+=` to `-=` in `sub_be`
  (the byte is read through `as u8`, and 256 is zero modulo 256), and `>` to `>=` in `advance_nonce_past` (the
  only differing case re-inserts the value already stored).

Result: of 247 viable mutants, **241 killed and 6 documented equivalents, none unexplained**. These were gaps in
the tests, not defects in the contract's behaviour.

### F4. Faults deliberately injected, to show the tests would notice

These are *not* defects that existed. Each was a deliberate change to production code, run against the named
check, and reverted byte for byte (checked by sha256):

| Injected fault | Caught by |
| --- | --- |
| The guardian allowed to call `set_asset_policy` | the `guardian` suite ("a forbidden call was refused for another reason") |
| The rule that the guardian cannot cancel a change to itself removed | the `guardian` suite ("a guardian cancelling a change to the guardian was accepted") |
| A guardian removal classified as a tightening | the `guardian` suite ("set_guardian: a loosening change took effect instantly") |
| The window ring forgetting a spend one slot early, or counting only the current slot | the loss-bound check (a greedy agent books 150 inside one real window against a limit of 100) and the stateful harness's ring oracle |
| `total <= window_limit` weakened to `total <= window_limit + 1` | Kani `allow_implies_every_limit_holds` fails on both limit assertions |
| The six mutants of F3 | the Kani classifier harnesses, by the direction of the iff (over-classification by completeness, under-classification by soundness), and the unit tests |
| The F2 fix reverted | the `accounting` and `policy` suites and the random driver |

### F5. Operational hazards from the live testnet run

Found on 2026-11-14 while running real payments through the deployed contract. These are configuration and
design-consequence findings, not contract defects, and they are recorded in the `note` field of
`web/client/src/data/live-run.json`.

- **Gas limits too low for the destination.** The first ETH payment (config `gas_limit` 21,000) went to an
  address with no balance. On this Sepolia, a value transfer to an empty account needed about 207,000 gas (the
  node's estimate), so the signed transaction was mined and **reverted**
  ([`0x9bd4d14c…0342`](https://sepolia.etherscan.io/tx/0x9bd4d14c8d02fbed56b6006a32f34963456bc6f7f85ba79e6f08325f44b30342)).
  The first USDC payment ran out of its 100,000 `token_gas_limit` (the node estimated 154,151) and also
  reverted
  ([`0x5910a8f3…fe22`](https://sepolia.etherscan.io/tx/0x5910a8f381ac51905b5c136c18b44f8c0cc66d5dc9444795f3f44d31a0d4fe22)).
- **What the contract did, as designed.** It signed, booked the spend (the G1 rule) and consumed the nonce, even
  though the transaction reverted on chain: the contract is outbound-only and cannot see the outcome (L5). The
  committed USDC usage read `1,000,000` (1 USDC) after two 0.5 USDC payments, one of which reverted.
- **Remediation.** The owner raised `token_gas_limit` to 200,000 through the timelock (`propose_change`, wait the
  delay, `execute_change`), and paid a funded EOA; both retries settled. The lesson is operational: size the gas
  limits to the destination, including receiving an empty account.

## 5. Known limitations and accepted risks

These are things the design does not protect against, or protects against only partly. They are listed so a
reviewer does not have to find them.

| # | Limitation | Detail and bound |
| --- | --- | --- |
| L1 | **G2: freeze and revoke cannot recall an already-signed EVM transaction** | An EIP-1559 transaction has no expiry; once signed, anyone can broadcast it until its nonce is mined or replaced. The loss is bounded by the limits that applied at signing (per-transaction cap, window limit, period budget). The owner can sign a replacement at the same nonce with `owner_evm_withdraw` (fees first raised, which is itself a loosening and waits out the timelock); this is a race against the broadcaster, a mitigation and not a guarantee. An automatic ledger was considered and rejected (`DESIGN.md`, "G2") |
| L2 | **In this demo deployment, one operator holds both the owner and the guardian keys** | This is a property of the *deployment*, not of the design. The guardian is designed to be a separate party: it is a distinct account (`init` and `set_guardian` refuse the owner and the contract account), its powers are exactly the three stop-powers, so it can be held by an independent off-chain monitor without being trusted with funds, and the design counts on that (`DESIGN.md`, "Guardian"). The contract cannot, however, check that two accounts belong to different people. Here a single operator holds both keys, and the timelock delay is 120 seconds, so in this deployment the reaction window is a mechanism that is demonstrated (the tests and the live deployment exercise it), not an independent defence. A real deployment would put the owner behind a multisig, the guardian with an independent monitor, and set a delay measured in hours. The design-level fact behind it is stated in `DESIGN.md`: a delay helps only if someone other than the key holder is watching and can cancel, and a hostile owner and guardian acting together can still drain after the delay |
| L3 | **Revoke-then-re-grant walks around the extension rule** | Revoke and a grant after a revoke are instant, so the rule "extending a live mandate is a loosening" can be bypassed by revoking first, at the cost of visibly dropping the agent's authority (`DESIGN.md`, "Owner timelock", limit (a)). The re-grant is still bounded by the un-loosened policy |
| L4 | **Instant scope-widening re-grants** | A grant of any scope is instant by design. This is safe because effective authority is policy ∩ scope ⊆ policy, and the policy cannot be widened without the delay; it is documented as deliberate, and rests on the Kani proofs of T5 and the classifier |
| L5 | **The contract is outbound-only; it cannot see the destination chain** | (a) **EVM nonce gaps:** a signing that fails or times out consumes its nonce; later transactions from the address queue behind it until the owner fills it. (b) **Budget books at signing, not at settlement:** a transaction that is signed but never broadcast, or that reverts on chain, still counts against the budget (F5 shows a real case). This over-counts and never under-counts. (c) A token may revert, charge a fee or return nothing; the contract cannot know |
| L6 | **Owner-set gas limits** | A gas limit too low for the destination wastes budget and consumes nonces (F5). The contract accounts fees at the worst case, which over-counts |
| L7 | **Chain Signatures is trusted for availability and for key derivation** | A signer outage fails closed (nothing signed, nothing booked), but halts EVM payments. That the derived address is controlled only by calls from this contract is a property of Chain Signatures, not something the contract proves; the contract checks only that signatures recover to the configured `sender` |
| L8 | **The agent key can burn its gas allowance** | The 0.25 NEAR allowance is spendable from the contract's balance, including on denied calls. It does not reset; re-issuing the key restores it. It bounds the fees the key can burn, not the funds it can move |
| L9 | **Proof bounds and what is not proved** | See below |
| L10 | **Keyless means no upgrade path; the owner is permanent** | Nobody can redeploy, so a bug cannot be fixed in place: the remedy is a new account and the owner's recovery methods. The signer of the deployment is the owner permanently (there is no `set_owner`) |
| L11 | **Not independently audited; testnet only** | This is the author's review. The contract has never been deployed to mainnet and should not hold real funds |
| L12 | **Smaller accepted limits** | EVM addresses are accepted as `0x` plus 40 hex digits without checking an EIP-55 checksum (the owner supplies the allowlist). Requests rejected before any policy is consulted (a malformed address, no EVM configuration) revert without a decision event. `num_buckets` is capped at 64, scope lists at 64 entries, pending changes at 32. A request with too little prepaid gas reverts with nothing reserved. Freezing does not recall in-flight transfers (they were allowed when requested and settle) |

### L9. What the proofs do not cover

- **The window proofs are bounded.** They use 3 spends at arbitrary non-decreasing times and amounts (`u8`, so the
  true sum cannot overflow), `K` fixed to 1, 2 or 3 slots per proof, slot lengths 1 to 8 ns, and times below 1,600
  (about 200 slots, far past the ring's span, so every index wrap and expiry is reached). Wider bounds (slot
  lengths to 2^20, times to 2^32) did not finish; a symbolic `K` or full 64-bit times exhausted memory.
- **The `evaluate` proofs are bounded.** The calling key is one of two fixed concrete keys or none; the asset is
  `Near` or `Evm(1)` (`Nep141` and `Evm20` are not exercised); a scope's asset list holds 0 or 1 entries; recipients
  are `bob.near` or `cat.near`. **The EVM recipient scope is not proved**, only unit-tested and covered by the
  model harness. Account ids are built without validation.
- **`PublicKey` equality is stubbed in the proofs** (a comparison of the curve tag and the first data byte, exact
  for the two fixed keys). That the standard derived `==` on a key is a faithful equality is *trusted, not proved*.
- **`encode_signed` and `try_assemble_signed` panic-freedom end to end is not proved.** The RLP integer encoder
  trims a symbolic number of leading zeros and copies a symbolic-length slice, and the model checker exhausted
  memory (11 GB) on every harness that made an integer or signature symbolic. Those harnesses were removed rather
  than left failing. The floor for that part is the 1,743-case differential corpus and the byte-level fuzz tests.
- **A semantic form of the fee rule was tried and removed.** That a higher accounted fee (max fee times gas
  limit) needs a higher factor was not proved; what is proved is the per-field strict comparison of the four
  bounds over their full widths, which is what the code checks.
- **The classifier proofs trust the state.** Parsing of addresses returns a symbolic 20 bytes (the proofs are about
  classification given what parsing returns); the EVM destination is compared as 20 bytes where the code used to
  compare the text (equivalent given tested round-tripping and normalized treasuries); and the lookups in the
  `Minder` implementation of `LooseningState` are exercised by tests, the model harness and mutation testing, not
  proved. Optional accounts are drawn from `{none, a.near, b.near}`.
- **The accounting is not formally proved.** The reserve/settle/release logic in `lib.rs` and the booking rule are
  covered by the model harness, the sandbox tests and the unit tests, and 28 mutants of the accounting helpers in
  `lib.rs` are not scored by mutation testing (deferred to a separate pass against the harness).
- **The mutation score is for a scope that has since changed.** It was measured before the classifier moved to
  `src/loosening.rs`; the current scope (294 mutants) has not been re-run. The classifier's coverage is now the
  Kani proofs.
- **Bounded model checking is not a proof for all inputs.** Every Kani result is a statement about the stated
  bounds; the stateful harness explores to depth 8 and samples beyond it.

## 6. Verification summary and proof ledger

### Kani: 43 harnesses (Kani 0.68.0)

`cargo kani list` reports 43 harnesses. `DESIGN.md` records each as verified with 0 failed checks, and all 43 were
**re-run for this review on 2026-11-14: 43 successfully verified, 0 failures** (30.5 minutes of wall-clock time),
with all 24 `kani::cover!` points satisfied (so no proof is vacuous) and none unsatisfiable. Source: `contract/kani/src/proofs.rs` (18) and
`contract/kani/src/loosening_proofs.rs` (25).

| Area | Harness | Property | Stated bounds |
| --- | --- | --- | --- |
| **Decision** (7) | `allow_implies_every_limit_holds`, `allow_implies_every_limit_holds_with_a_recipient_scope` | If `evaluate` returns Allow, the contract is active, the mandate exists, is unrevoked and unexpired, the key is the mandate's, the asset has a policy and the recipient is allowlisted; `amount <= per_tx_cap`; the window and budget hold with no wraparound; and any scope names the asset and recipient and its cap holds | Symbolic status, `now`, `expiry`, every `u128` field; key one of two fixed keys or none; asset `Near` or `Evm(1)`; scope lists 0 or 1 entries; recipient `bob.near` or `cat.near`; EVM recipient scope not exercised; `PublicKey` equality stubbed |
| | `a_scope_never_widens_authority`, `a_scope_never_widens_authority_with_a_recipient_scope` | If a scoped evaluation allows, the same request under an unscoped mandate allows | the same |
| | `frozen_revoked_expired_and_wrong_key_always_deny` | Frozen, no mandate, revoked, expired, or a wrong or missing key each yield Deny with exactly that reason, whatever else holds | the same |
| | `the_deny_reason_is_the_first_failing_check_in_the_documented_order`, `..._with_a_recipient_scope` | The decision equals the documented chain of checks, Allow only when none fails | the same |
| **Timelock classifier** (25) | For each of 12 `OwnerChange` variants: a **soundness** harness (a loosening is never classified instant) and a **completeness** harness (classified loosening implies the spec), e.g. `a_policy_that_admits_more_is_never_instant` / `a_policy_that_is_not_loosening_admits_nothing_new`; plus `a_destination_is_safe_only_if_it_is_the_treasury_or_the_sender` | `loosens` equals a per-variant spec written in terms of what a change does to authority. For `SetAssetPolicy` the oracle is the real `policy::evaluate` | Numbers, flags and bytes fully symbolic (`u64`, `u128`, `[u8; 20]`); a recipient allowlist is one symbolic bool per element, so set-valued dimensions have **no bound**; optional accounts from `{none, a.near, b.near}`; parsing and the `Minder` lookups trusted. 25 harnesses, about 218 s of solver time, every `kani::cover!` satisfied (not vacuous) |
| **Sliding window** (6) | `the_ring_never_under_counts_with_1_slot` (and `_2_slots`, `_3_slots`) | The ring counts at least the true spend inside the window (the fail-safe direction) | 3 spends at arbitrary non-decreasing times and `u8` amounts; `K` = 1, 2 or 3; slot length 1 to 8 ns; times below 1,600 |
| | `the_ring_holds_spend_at_most_one_slot_too_long_with_1_slot` (and `_2_slots`, `_3_slots`) | And at most the spend made within one slot beyond the window | the same |
| **EVM normalization and encoding** (5) | `normalize_s_is_total_and_always_low_s` | For every `s` in `1..n` and recovery id 0 or 1, no panic, `1 <= s' <= n/2`, parity 0 or 1; a low `s` passes through, otherwise `s' = n - s` and the parity flips | none: all 2^256 values of `s` |
| | `normalized_signature_is_total_and_yields_only_valid_signatures` | No panic; an accepted signature has `1 <= r < n`, `1 <= s <= n/2`, parity 0 or 1, from a 33-byte point (prefix 02 or 03) and a 32-byte scalar | lengths 0 to 34 bytes (longer take the same early return); bytes fully symbolic |
| | `encoding_any_recipient_never_panics`, `encoding_any_55_bytes_of_data_never_panics`, `encoding_any_56_bytes_of_data_never_panics` | Encoding (unsigned and signed) never panics for any recipient, and for calldata of the longest short RLP string (55) and the shortest long one (56) | the rest of the transaction is a fixed base transfer |

### Mutation testing: `cargo-mutants` 27.1.0, rustc 1.94.1

| | Count |
| --- | --- |
| Mutants measured | **259** (`policy.rs` 30, `window.rs` 17, `timelock.rs` 73, `gate.rs` 26, `evm.rs` 113) |
| Unviable (do not compile) | 12 |
| Viable | **247** |
| Caught initially | 235 |
| Missed initially | 12 |
| **Killed after triage** | **241** (six gaps closed by four new tests) |
| **Documented equivalents** | **6** (none unexplained) |

Test set: `cargo test --lib -- --skip model_tests` (the unit tests, about 25 s). **Caveat:** this was measured before
the classifier moved to `src/loosening.rs`. `contract/.cargo/mutants.toml` now scopes 294 mutants (64 in
`loosening.rs`, 44 in `timelock.rs`); that scope has **not** been re-run. The classifier is now covered by the
Kani proofs, which supersede the mutation score for it.

### Differential corpus: 1,743 cases

`contract/vectors/gen-evm-corpus.mjs` generates a seeded corpus; `tests/evm_corpus.rs` requires the Rust encoder to
match viem 2.57.16 byte for byte on the unsigned bytes, the signing hash and the signed bytes, including integer
widths and RLP edges. It includes 300 random and 40 edge ERC-20 transfers (the calldata is rebuilt and compared
340 times) and data lengths from 0 to 1,000 bytes. `evm.rs` also carries its own twelve viem vectors.

### Stateful model-based harness (`src/model_tests`)

The contract is run against an independently written shadow model (`model.rs` restates the rules from the
documentation: spend is a plain list of `(time, amount)`; the window is arithmetic on slot numbers). The exhaustive
driver is a depth-first search that deduplicates states.

| Suite | States | Transitions |
| --- | --- | --- |
| accounting | 22,857 | 91,479 |
| authority | 30,715 | 143,154 |
| policy | 29,991 | 147,137 |
| scope | 30,508 | 126,329 |
| budget | 98,543 | 391,797 |
| token | 19,178 | 90,716 |
| timelock | 38,605 | 129,199 |
| guardian | 65,036 | 242,233 |
| budget reset | 73,949 | 235,669 |
| **Total** | **409,382** | **1,597,713** |

Plus a random driver of 4,000 proptest cases of up to 40 actions over the full alphabet from a fixed seed, and
byte-level fuzz tests at 10,000 cases per property. Every driver asserts coverage, so it cannot pass by doing
nothing.

### Test suite

`contract/test.sh` runs the whole suite. It was **re-run for this review on 2026-11-14 and passed (exit 0), 418
tests in about 14 minutes**: 297 unit (including the model-based harness), 17 `agent_key`, 11 `custody`, 6
`deploy`, 2 `evm_corpus`, 38 `evm_gate`, 15 `invariants`, 4 `method_surface`, 9 `signing_confinement`, 11
`timelock` and 8 `transfers`. These are the same counts `DESIGN.md` records for its last full run.

### Reproducible-build result

`scripts/verify-deployment.sh` builds the wasm from a clean `git archive` of the committed tree and compares its
sha256 with the code the chain holds for the contract account. It reproduced the deployed hash
`2fdd044411312fba8bc18b91ce06138b0c933cdb32bbb5296964f6b35ba54d09` (428,360 bytes) with rustc 1.94.1 and
cargo-near 0.23.0, both with the default cargo home and with a different one (the wasm embeds the cargo registry
path, so the script maps the cargo home onto the deployed one; without that, a different cargo home alone gave a
different, larger wasm). It also checks on chain that the account has **no full-access key** and that its one key
is limited to the account and the three request methods with a finite allowance.

What this does not establish: the build is not reproducible by construction (`cargo near build
non-reproducible-wasm`); the contract's on-chain metadata carries no build info (`"build_info": null`, `"link":
null`), so nothing on chain says what source produced the deployed code. Source-to-bytecode provenance rests on
the documented toolchain pins (`Cargo.lock` with `--locked`, `near-sdk =5.29.1`, rustc 1.94.1; `DESIGN.md` does
not pin cargo-near, and 0.23.0 is the version used here). Other compiler or cargo-near versions were not tried.

### Live run

The real run recorded in `web/client/src/data/live-run.json` (a 0.05 NEAR payment, a 0.0002 ETH payment and a 0.5
USDC payment, gated by the contract, signed through Chain Signatures, settled on Sepolia) comes with a provenance
check: the signer recovered from each raw transaction equals the contract's configured sender and the address
Chain Signatures derives for this contract account and path. The `/app` console recomputes it in the visitor's
browser; the same computation was run against the live chain for this review and all three addresses matched
(`0x73Cd4718d7577318D76Ab78db13a9Ec7CEcb7F3F`).

### Red-team log and freeze drill

Recorded against the live contract on 2026-11-14 and shown on `/app` (`web/client/src/data/red-team.json`,
`freeze-drill.json`). With the agent's real key: eight attacks refused on chain (over the cap on NEAR, ETH and USDC;
an unlisted recipient on all three; an asset with no policy, NEP-141 and ERC-20), each a failed transaction whose
log carries the contract's own decision event; a window burst (two allowed 0.5 NEAR payments, then a third refused
`WindowLimitExceeded`), which one dry run cannot show; and five calls the NEAR runtime refused before any contract
code ran, recorded as raw RPC errors because they never reach a block (an owner method, `AddKey`, `Transfer`,
`DeployContract` and a direct call to the signer). The freeze drill: the guardian froze the contract, the agent was
refused `Frozen` on chain, and the owner could resume only by proposing the unfreeze and waiting out the 120-second
timelock. The contract ended Active, and `scripts/verify-deployment.sh` still passed on the keys and the code.

## 7. Reproducing this review

```
# the deployed code and the keys (read-only; builds a clean copy)
scripts/verify-deployment.sh

# the proofs (Kani installs its own nightly toolchain)
cargo install --locked kani-verifier && cargo kani setup
cd contract/kani && cargo kani                 # all 43 harnesses

# the tests (about ten minutes; needs cargo-near)
contract/test.sh

# the mutation tests
cd contract && cargo mutants -j 1 --timeout 120 --build-timeout 900 \
    --file src/policy.rs --file src/window.rs --file src/loosening.rs \
    --file src/timelock.rs --file src/gate.rs --file src/evm.rs -- --lib -- --skip model_tests
```

### What was re-run for this review

On 2026-11-14 (the Kani and test runs against commit `dfe065c`; the deployment check was run at `92efc41`, and `contract/` is unchanged between the two):

| Check | Result |
| --- | --- |
| `cd contract/kani && cargo kani` | 43 of 43 harnesses verified, 0 failures; 24 cover points satisfied |
| `contract/test.sh` | all test binaries passed: 418 tests |
| `scripts/verify-deployment.sh`, clean build from the committed tree | PASS (4 of 4): built wasm equals the on-chain code (`2fdd0444…`), the repository's recorded hash equals the on-chain code, no full-access key, one restricted key |
| Citations in this document | every test and proof name cited in Section 3 and 4 was checked to exist as a function in `contract/` |

**Not re-run for this review:** the mutation tests (the figures in Section 6 are `DESIGN.md`'s measured results,
and the current 294-mutant scope has never been run) and the live Chain Signatures signing (the recorded run in
`live-run.json` is from 2026-11-14 and is verifiable on the explorers).
