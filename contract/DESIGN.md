# Minder policy core: design

Scope of this chunk: pure policy state and evaluation. It moves no value and
signs nothing. The reservation lifecycle and the cross-chain signing gate come
later and will call the same `evaluate` function.

## Layout

| File | Role |
| --- | --- |
| `src/types.rs` | `AssetId`, `Status`, `Config`, `AssetPolicy`, `Mandate`, `DenyReason`, `Decision` |
| `src/policy.rs` | `evaluate(&EvalInput) -> Decision`: pure, no state, no I/O. The only place policy rules live |
| `src/window.rs` | `SpendRing`: the sliding window. Pure; takes `now` and `slot_len` explicitly |
| `src/lib.rs` | Contract state, owner-only methods, views. Gathers state into `EvalInput` and calls `evaluate` |

## State

| Field | Type | Notes |
| --- | --- | --- |
| `config` | `Config { owner, status, window_len, num_buckets }` | Set once in `init`; only `status` changes later |
| `mandate` | `Option<Mandate { agent, expiry, epoch, revoked }>` | One mandate at a time |
| `last_mandate_epoch` | `u64` | Incremented on every grant, never reset, so epochs are never reused |
| `policies` | `IterableMap<AssetId, AssetPolicy>` | Having an entry allowlists the asset |
| `recipients` | `IterableSet<AccountId>` | Allowed recipients |
| `spend` | `LookupMap<AssetId, SpendRing>` | Per-asset window accounting |

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

## Evaluation order

The first failing check decides the reason: `Frozen`, `NoMandate`, `MandateRevoked`,
`MandateExpired` (`now >= expiry`), `NotAuthorized` (caller is not the mandated agent),
`AssetNotAllowed`, `RecipientNotAllowed`, `PerTxCapExceeded` (`amount > cap`),
`WindowLimitExceeded` (`live_spend + amount > limit`, or overflow).

## Choices that need your review

- **`dry_run` takes a `caller` argument** (spec: `asset, amount, recipient`). A view has no
  authenticated caller, so without it `NotAuthorized` could never be produced. The real action
  path will pass the predecessor.
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
- **`test-helpers` feature** adds the owner-only `test_record_spend`, which stands in for the
  settle step. The integration suite builds its own wasm with it. `cargo near build` without
  the feature does not export it (checked).

## Invariant to test map

Unit tests are in `src/window.rs` and `src/policy.rs`. Integration tests are in
`tests/invariants.rs` and run against a near-workspaces sandbox.

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

Sandbox time does not track block counts precisely, so no integration test asserts an exact time
boundary.

## Running

```
cargo near build non-reproducible-wasm --locked   # production wasm, no test helper
cargo test                                        # unit + sandbox tests; builds a test wasm itself
```
