# Minder: design

Scope so far: pure policy state and evaluation, and the local NEAR enforcement path (the agent moves
NEAR or NEP-141 tokens held by this contract, gated by policy, with a reserve/settle/release
lifecycle). The cross-chain signing gate and access-key mandates are not built yet.

## Layout

| File | Role |
| --- | --- |
| `src/types.rs` | `AssetId`, `Status`, `Config`, `AssetPolicy`, `Mandate`, `DenyReason`, `Decision` |
| `src/policy.rs` | `evaluate(&EvalInput) -> Decision`: pure, no state, no I/O. The only place policy rules live |
| `src/window.rs` | `SpendRing`: the sliding window. Pure; takes `now` and `slot_len` explicitly |
| `src/lib.rs` | Contract state, owner-only methods, `request_local_transfer`, `on_transfer_complete`, views. Gathers state into `EvalInput` and calls `evaluate` |
| `src/lifecycle_tests.rs` | Unit tests for reserve, dispatch and settlement under the mocked environment |
| `../test-contracts/mock-ft` | Test double for a NEP-141 token (records `ft_transfer`, can fail or stretch it over blocks). Never deployed |

## State

| Field | Type | Notes |
| --- | --- | --- |
| `config` | `Config { owner, status, window_len, num_buckets }` | Set once in `init`; only `status` changes later |
| `mandate` | `Option<Mandate { agent, expiry, epoch, revoked }>` | One mandate at a time |
| `last_mandate_epoch` | `u64` | Incremented on every grant, never reset, so epochs are never reused |
| `policies` | `IterableMap<AssetId, AssetPolicy>` | Having an entry allowlists the asset |
| `recipients` | `IterableSet<AccountId>` | Allowed recipients |
| `spend` | `LookupMap<AssetId, SpendRing>` | Per-asset committed window accounting |
| `reserved` | `LookupMap<AssetId, u128>` | Per-asset amount reserved for transfers still in flight. Absent means zero |

`AssetId` is `Near | Nep141(AccountId)`. The EVM variant is appended later.

Single initialization comes from `#[init]`, which panics when state exists.

## Sliding window algorithm

`slot_len = window_len / num_buckets`, and `init` rejects a `window_len` not divisible by
`num_buckets`. The slot of a timestamp is `now / slot_len`. A `SpendRing` for a window of `k =
num_buckets` slots holds **`k + 1`** internal buckets of `(slot, amount)`, with the bucket for slot
`s` at index `s % (k + 1)`. `k` and `window_len` are the user-facing values; the extra bucket is
internal.

- **Live:** a bucket is live when `0 <= current_slot - bucket.slot <= k`, so spend counts for `k + 1`
  slots: the one it was made in plus the `k` after it.
- **Why `k + 1` buckets:** the oldest live slot (age `k`) must coexist with the current slot. With only
  `k` buckets they would share a ring index. With `k + 1`, a bucket is reused only once its old slot is
  past age `k`, so nothing live is ever overwritten.
- **Read (`live_sum`):** sums live buckets and mutates nothing, so it works in views. Expired buckets
  are skipped, which is equivalent to zeroing them.
- **Write (`record`):** if the target bucket holds an older slot it is zeroed first, then the amount
  is added.
- **Tolerance:** spend recorded at `t` stops counting between `t + window` and `t + window + slot`.
  **The window never under-enforces the limit and over-enforces by at most one slot width.** The
  approximation errs towards denying. The fixed window is the `k = 1` case, where spend counts for
  two slots.
- **Overflow:** sums saturate. A saturated value exceeds every limit, so it fails closed.

## Effective spend and the reserved counter

`reserved[asset]` is the sum of amounts for transfers that have been requested and not yet settled.
It is kept apart from the window ring, which holds only committed spend.

**Effective spend = `ring.live_sum(now) + reserved[asset]`** (saturating). It is the only spend figure
`evaluate` sees (`EvalInput.effective_spend`, built by `policy::effective_spend`). `dry_run` and
`request_local_transfer` both go through `Minder::evaluate_request`, so a view always agrees with the
real path, in-flight reservations included. `get_live_spend` still reports committed spend only;
`get_reserved` reports the reservation.

## Reservation lifecycle

`request_local_transfer(asset, amount, recipient)` is called by the agent:

1. Build the evaluation input (caller is the predecessor) and call `evaluate`. On `Deny` the method
   panics with `Denied: <Reason>`, so nothing is reserved and no promise exists.
2. On `Allow`, `reserved[asset] += amount` (saturating) **before any promise is created**. A call that
   interleaves later, in a later receipt, therefore already sees the reservation. This is what stops
   a burst of requests from each passing the limit against the same unspent budget.
3. Dispatch the transfer: `AssetId::Near` is a native `Promise::transfer`; `AssetId::Nep141(token)`
   is a plain `ft_transfer` on `token` with 1 yoctoNEAR. Never `ft_transfer_call`.
4. Chain `on_transfer_complete(asset, amount)` with `.then` and return the promise.

The reservation exists on chain only if the method returns normally. Any panic after step 2 (for
example the promise cannot be created for lack of gas) reverts the whole receipt, reservation
included.

`on_transfer_complete` is `#[private]` (the predecessor must be the contract account) and **must
never panic**, because a panic would leave `reserved` inflated for good. In order:

1. `reserved[asset] -= amount`, saturating, unconditionally. The entry is removed at zero.
2. Read the transfer result. Unless it is a definite failure, `ring.record(now, slot_len, amount)`
   commits the spend into the window at settle time.

Steps 1 and 2 are one receipt, so effective spend does not move when a transfer succeeds: the amount
leaves `reserved` and enters the ring atomically. On failure it leaves `reserved` and enters nothing,
which returns the budget. NEAR runs a callback exactly once, and the code relies on that for
exactly-once settlement.

## Evaluation order

The first failing check decides the reason: `Frozen`, `NoMandate`, `MandateRevoked`,
`MandateExpired` (`now >= expiry`), `NotAuthorized` (caller is not the mandated agent),
`AssetNotAllowed`, `RecipientNotAllowed`, `PerTxCapExceeded` (`amount > cap`),
`WindowLimitExceeded` (`effective_spend + amount > limit`, or overflow).

## Choices that need your review

- **`dry_run` takes a `caller` argument** (spec: `asset, amount, recipient`). A view has no
  authenticated caller, so without it `NotAuthorized` could never be produced.
  `request_local_transfer` passes the predecessor instead.
- **The agent is an `AccountId`**, not an access key. Key-based mandates (function-call key on the
  contract account) are a later chunk.
- **`init` caller:** must be the owner or the contract account itself, so nobody else can claim
  ownership by initializing first. Deploy and init in one batched transaction.
- **`NoMandate` and `MandateRevoked`** are separate reasons so an audit can tell "never granted"
  from "revoked".
- **Spend history survives `remove_asset_policy`**, so removing and re-adding a policy cannot reset
  the window.
- **`num_buckets` is capped at 64** to bound per-asset storage and per-evaluation work.
- **`revoke_mandate` reverts with no mandate; `remove_asset_policy` reverts with no policy;** other
  owner methods are idempotent.
- **Dependency pins:** `near-sdk =5.29.1` and `near-workspaces =0.23.0`, with `near-sdk-env 0.1.6`
  and `near-sys 0.2.14` in `Cargo.lock`. Newer releases require rustc 1.95; this environment has
  1.94.1.
- **`test-helpers` feature** adds the owner-only `test_record_spend`, which seeds committed spend
  without moving value. The integration suite builds its own wasm with it. `cargo near build`
  without the feature does not export it (checked).
- **Commit unless the transfer definitely failed.** The callback skips the commit only for
  `PromiseError::Failed`. A success with an unexpected payload (`TooLong`) still commits. Over-counting
  merely tightens the budget for one window; forgetting a transfer that happened would let the limit
  be exceeded.
- **Callback gas is fixed and reserved up front:** 30 Tgas for `ft_transfer` and 15 Tgas for
  `on_transfer_complete`. The request needs at least their sum plus its own cost prepaid (about 50
  Tgas); with less, promise creation fails and the whole request reverts. The callback's work (a few
  storage reads and writes, one ring of at most 65 buckets) is far below 15 Tgas, so it cannot run
  out and strand a reservation.
- **Funds live in this contract.** Native transfers pay from the contract's own balance, and a
  transfer that would breach the storage-staking floor fails like any other failed transfer. Nothing
  here funds or withdraws; that is owner tooling for later.
- **Freezing or removing a policy does not recall in-flight transfers.** They were allowed when
  requested and are already dispatched; their callbacks still settle. Only new requests are denied.
- **The commit slot is the settle slot,** not the request slot. The reservation covers the gap, so
  effective spend never dips between request and settlement.
- **`request_local_transfer` is not payable,** so near-sdk rejects any attached deposit.
- **`EvalInput.live_spend` was renamed `effective_spend`** to say what it now holds.

## Invariant to test map

Unit tests are in `src/window.rs`, `src/policy.rs` and `src/lifecycle_tests.rs`. Integration tests
are in `tests/invariants.rs` (policy) and `tests/transfers.rs` (local transfer path), and run against
a near-workspaces sandbox.

| Invariant | Test |
| --- | --- |
| Per-tx cap -> `PerTxCapExceeded` | `amount_over_per_tx_cap_is_denied`; unit `policy::caps_are_inclusive` |
| Cumulative spend over window limit -> `WindowLimitExceeded`; the window never under-enforces the limit and over-enforces by at most one slot width | `cumulative_spend_within_one_window_is_capped`; unit `policy::caps_are_inclusive`, `overflowing_total_is_denied` |
| Spend stops counting after a full window | `spend_stops_counting_once_a_full_window_has_passed` (coarse, advances well past the window) |
| Exact sub-bucket boundary: never forgotten early, at most one slot late | unit `window::spend_is_live_for_k_plus_one_slots`, `spend_is_never_forgotten_early_and_at_most_one_slot_late`, `single_bucket_window_counts_for_two_slots`, `oldest_live_slot_does_not_alias_the_current_slot`, `reused_bucket_is_zeroed_before_adding`, `live_sum_adds_across_buckets_and_ages_them_out_one_at_a_time`, `sum_saturates_instead_of_wrapping` |
| Spend history survives policy removal | `reinstating_a_policy_keeps_spend_history` |
| Recipient not allowlisted -> `RecipientNotAllowed` | `recipient_not_on_allowlist_is_denied` |
| No policy -> `AssetNotAllowed` | `asset_without_a_policy_is_denied` |
| Non-agent caller -> `NotAuthorized` | `only_the_mandated_agent_is_authorized` |
| Non-owner calls to owner-only methods revert | `owner_only_methods_revert_for_everyone_else` |
| Second `init` reverts; takeover by outsider reverts | `second_init_reverts`, `init_rejects_callers_other_than_owner_or_contract_and_bad_windows` |
| Revoked stays revoked until an explicit new grant | `revoked_mandate_stays_revoked_until_a_new_grant`, `no_mandate_is_denied_and_revoking_nothing_reverts` |
| Expired stays expired until an explicit new grant; past expiry cannot be granted | `expired_mandate_stays_expired_until_a_new_grant`; unit `policy::expiry_is_exclusive_of_the_expiry_instant` |
| Frozen denies everything until `unfreeze` | `frozen_contract_denies_everything_until_unfrozen`; unit `policy::frozen_outranks_every_other_check` |
| Getters report state | `getters_report_configuration` |
| Effective spend includes reservations | unit `policy::effective_spend_includes_reservations` |
| TOCTOU: a reservation is counted before the promise exists, so later calls see it | unit `lifecycle_tests::reservation_is_visible_before_the_callback_runs`, `a_call_made_while_another_is_in_flight_cannot_exceed_the_limit`; sandbox `transfers::reservation_is_visible_and_binding_while_the_transfer_is_in_flight` (observes the reservation mid-flight, `dry_run` honours it) |
| Reservations are per asset | unit `lifecycle_tests::reservations_are_tracked_per_asset` |
| Dispatch: native `transfer`, plain `ft_transfer` with 1 yocto, never `ft_transfer_call`, callback args and gas | unit `lifecycle_tests::native_request_creates_a_transfer_then_a_private_callback`, `token_request_calls_plain_ft_transfer_with_one_yocto`; sandbox `transfers::token_transfer_calls_ft_transfer_and_commits_the_spend_once` (mock insists on 1 yocto and has no `ft_transfer_call`) |
| Exactly-once settlement: a successful transfer commits the amount once | unit `lifecycle_tests::successful_transfer_moves_the_reservation_into_the_window_once`; sandbox `transfers::native_transfer_pays_the_recipient_and_commits_the_spend_once`, `token_transfer_calls_ft_transfer_and_commits_the_spend_once` |
| Clean reversion: a failed transfer commits nothing and the budget returns | unit `lifecycle_tests::failed_transfer_releases_the_reservation_and_commits_nothing`; sandbox `transfers::failed_native_transfer_restores_the_budget` (nonexistent recipient), `failed_token_transfer_restores_the_budget` (mock `ft_transfer` panics) |
| Callback never panics, and never over-releases | unit `lifecycle_tests::settlement_never_panics_even_if_it_releases_more_than_is_reserved`, `settlement_is_independent_of_policy_changes_made_in_flight` |
| Ambiguous success still counts as spent | unit `lifecycle_tests::an_unexpected_success_payload_still_counts_as_spent` |
| No stranded reservation: a denied or reverted request leaves `reserved` unchanged | unit `lifecycle_tests::a_denied_request_reserves_nothing`; sandbox `transfers::denied_request_reverts_and_reserves_nothing`, `request_without_gas_for_the_callback_reverts_and_reserves_nothing` |
| Callback is private | sandbox `transfers::only_the_contract_itself_can_call_the_settlement_callback` (the `#[private]` guard lives in the generated entry point, so unit tests cannot reach it) |

Sandbox time does not track block counts precisely, so no integration test asserts an exact time
boundary.

## Running

```
cargo near build non-reproducible-wasm --locked   # production wasm, no test helper
cargo test                                        # unit + sandbox tests; builds the test wasms itself
```
