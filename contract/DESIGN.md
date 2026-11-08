# Minder: design

Scope so far: pure policy state and evaluation, the local NEAR enforcement path (the agent moves
NEAR or NEP-141 tokens held by this contract, gated by policy, with a reserve/settle/release
lifecycle), fund custody (owner withdrawal and deposits), and the pure EVM transaction encoder
that the cross-chain signing gate will use. The gate itself (MPC requests, yield/resume, state,
wiring) and access-key mandates are not built yet.

## Layout

| File | Role |
| --- | --- |
| `src/types.rs` | `AssetId`, `Status`, `Config`, `AssetPolicy`, `Mandate`, `DenyReason`, `Decision` |
| `src/policy.rs` | `evaluate(&EvalInput) -> Decision`: pure, no state, no I/O. The only place policy rules live |
| `src/window.rs` | `SpendRing`: the sliding window. Pure; takes `now` and `slot_len` explicitly |
| `src/evm.rs` | Pure EIP-1559 transfer encoding: `Eip1559Tx`, `encode_unsigned`, `signing_hash`, `normalize_s`, `encode_signed`. No state, not yet called by any contract method |
| `vectors/gen-evm-vectors.mjs` | Generates the reference serializations (viem) that `src/evm.rs` tests hard-code |
| `src/lib.rs` | Contract state, owner-only methods, `owner_withdraw`, `deposit`, `ft_on_transfer`, `request_local_transfer`, `on_transfer_complete`, views. Gathers state into `EvalInput` and calls `evaluate`. `transfer_promise` builds the transfer for both the agent path and owner withdrawal |
| `src/lifecycle_tests.rs`, `src/custody_tests.rs` | Unit tests under the mocked environment: reserve/dispatch/settlement, and custody |
| `src/test_support.rs` | Fixtures shared by those unit tests |
| `../test-contracts/mock-ft` | Test double for a NEP-141 token: records `ft_transfer`, can fail or stretch it over blocks, and `ft_transfer_call` drives a receiver's `ft_on_transfer`. Never deployed |

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

## Fund custody

This contract holds the funds the agent can spend: NEAR in its own balance, and NEP-141 tokens in
its account at each token contract. It keeps no ledger of them; the balances are the chain's.

### Threat-model boundary

**Minder contains the untrusted agent, not the owner.** The owner owns the treasury, so
`owner_withdraw` is intentionally unrestricted. Defending against a compromised owner key is out of
MVP scope; the Tier 2 policy-change timelock is the eventual mitigation.

### `owner_withdraw(asset, amount, to) -> Promise`

- **Owner-only** (`assert_owner`).
- **Works in any status, including `Frozen`, and with the mandate revoked or expired.** This is the
  owner-recovery property: freezing stops the agent, never the owner.
- **Not subject to policy.** No per-transaction cap, no window limit, no recipient allowlist, and
  the asset needs no policy. It does not read or modify `reserved` or the spend ring, and calls
  neither `evaluate` nor `evaluate_request`.
- Dispatches through the same `transfer_promise` as the agent path: a native transfer, or a plain
  `ft_transfer` with 1 yoctoNEAR (needs the same 30 Tgas for the token call). **No callback**, since
  there is no accounting to settle; a failed transfer fails the returned promise. A native
  withdrawal that would take the balance below the storage-staking floor fails the same way.

### Deposits

- **`deposit()` (payable, anyone):** the runtime credits the attached NEAR to the contract's
  balance; the body only rejects a zero deposit and logs. It exists to make funding explicit. Plain
  transfers to the account also work.
- **`ft_on_transfer(sender_id, amount, msg) -> U128` (NEP-141 receiver hook):** called by a token on
  `ft_transfer_call`. The caller, `env::predecessor_account_id()`, is the token. If
  `AssetId::Nep141(caller)` has a policy it returns `0` (everything kept); otherwise it returns
  `amount` (full refund). `sender_id` and `msg` are ignored, because anyone can fill them in and
  they carry no authority; only the caller's identity is checked. The hook changes no state, so it
  cannot touch `reserved` or the window.
- **Storage registration is a precondition.** The contract account must be storage-registered on each
  token (`storage_deposit` on the token with `account_id` set to this contract) before the token
  will credit it. Without that the token rejects the transfer and `ft_on_transfer` is never reached.
  Registration is a call to the token, not a Minder method; anyone can pay for it.
- **Only `ft_transfer_call` can be filtered.** A plain `ft_transfer` to a registered contract skips
  `ft_on_transfer`, so tokens without a policy can still arrive that way. They are harmless and the
  owner can withdraw them.

## EVM transaction module (`src/evm.rs`)

Pure functions over plain data, written for the cross-chain signing gate. The module has no state
and does no I/O. It is exported but no contract method calls it yet; the spend path, MPC request
and broadcast come in later sub-chunks.

### Type

`Eip1559Tx { chain_id: u64, nonce: u64, max_priority_fee_per_gas: u128, max_fee_per_gas: u128,
gas_limit: u64, to: [u8; 20], value: u128 }`. A native ETH transfer only: **data and access list are
empty** (ERC-20 calldata is a later sub-chunk). Widths follow the Ethereum fields: `u128` holds any
wei value or fee that fits in practice, `u64` holds nonce, gas and chain id.

### Functions

| Function | Result |
| --- | --- |
| `encode_unsigned(&tx)` | `0x02 \|\| rlp([chain_id, nonce, max_priority_fee_per_gas, max_fee_per_gas, gas_limit, to, value, data, access_list])` with empty data and access list |
| `signing_hash(&tx)` | `keccak256(encode_unsigned(tx))`, the 32 bytes handed to the signer |
| `normalize_s(s, recovery_id)` | `(s, y_parity)`: if `s > n/2` then `s = n - s` and the parity flips, else unchanged |
| `encode_signed(&tx, r, s, y_parity)` | `0x02 \|\| rlp([...same fields, y_parity, r, s])`, ready to broadcast |

### Rules

- **No EIP-155.** A type-2 transaction carries its chain id inside the signed payload. `y_parity` is
  the recovery id itself (0 or 1): never `+ 27`, never `chain_id * 2 + 35`. This matches what the
  spike observed from chainsig.js (`yParity = v - 27`).
- **Low-s.** Ethereum rejects `s > n/2` (EIP-2). `n/2` is rounded down, so `s == n/2` is already low.
  Negating `s` negates the recovered point, which is why the parity flips with it. `normalize_s` must
  run before `encode_signed`.
- **Integers are RLP integers,** including `r` and `s`: big-endian, no leading zeros, zero is the
  empty string (`0x80`). A 32-byte `r` with leading zero bytes therefore encodes shorter.
- **RLP is hand-written** for exactly this structure (about 40 lines) rather than a crate: it avoids a
  dependency in a security contract and the vectors, not inspection, establish correctness. The
  generic length prefix is also checked against published RLP spec values for lengths this
  structure cannot reach.
- **Keccak is `env::keccak256_array`,** a deterministic host function (cheaper than a crate, nothing
  extra in the wasm). It is checked against the empty-string and `"abc"` Keccak vectors and the
  viem signing hashes.
- **Invalid input panics:** `normalize_s` rejects `recovery_id > 1` and `s` outside `1..n`;
  `encode_signed` rejects `y_parity > 1`. Better a loud failure than a transaction that recovers to
  another signer. The signing gate must validate the signer's output *before* calling these, because
  it may not panic inside a settlement callback.

### Reference vectors

The audit anchor is `vectors/gen-evm-vectors.mjs`, which uses viem 2.57.16 (an independent
implementation) to serialize fixed field sets with fixed signatures, and prints the unsigned bytes,
the signing hash and the signed bytes. The Rust tests hard-code those outputs and require byte-for-byte
equality for all three. Run it from a directory with viem installed (the spike has it) to reproduce.

| Vector | What it pins down |
| --- | --- |
| `plain_transfer` | A realistic Sepolia transfer; short-form unsigned list, long-form signed list |
| `zero_fields` | Zero nonce, priority fee and value encode as `0x80`; `r` and `s` with leading zero bytes are trimmed; `y_parity` 0 |
| `single_byte_boundaries` | 127 stays one raw byte, 128 becomes `0x81 0x80`, 255 `0x81 0xff`, 256 `0x82 0x01 0x00`; `r = 0x80` |
| `list_payload_55_bytes`, `list_payload_56_bytes` | The exact switch from short list header `0xf7` to long form `0xf8 0x38` |
| `high_top_bytes` | `u64::MAX`, `u128::MAX` and an all-`0xff` address and `r`: full-width, high top byte values |

A mutation check was run when writing these: deliberately breaking the list-length threshold, the
single-byte rule, leading-zero trimming, the type byte, the data encoding, the parity flip, the
half-order comparison and the borrow in the subtraction each made the tests fail.

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
  transfer that would breach the storage-staking floor fails like any other failed transfer. Funding
  and recovery are described under Fund custody.
- **Freezing or removing a policy does not recall in-flight transfers.** They were allowed when
  requested and are already dispatched; their callbacks still settle. Only new requests are denied.
- **The commit slot is the settle slot,** not the request slot. The reservation covers the gap, so
  effective spend never dips between request and settlement.
- **`request_local_transfer` is not payable,** so near-sdk rejects any attached deposit.
- **`EvalInput.live_spend` was renamed `effective_spend`** to say what it now holds.
- **`owner_withdraw` shares `transfer_promise` with the agent path** so there is one place that
  builds a transfer and "plain `ft_transfer`, never `ft_transfer_call`" is enforced once.
- **`ft_on_transfer` decides on policy existence only.** It does not check the amount, the sender or
  the status; a frozen contract still accepts allowlisted tokens, since funding is not agent spend.
- **The mock token has `ft_transfer_call`** only to exercise `ft_on_transfer`. The dispatch tests
  still show the agent path never uses it: the unit tests inspect the receipts, and the sandbox tests
  require the recipient to receive tokens through `ft_transfer`.

## Invariant to test map

Unit tests are in `src/window.rs`, `src/policy.rs`, `src/evm.rs`, `src/lifecycle_tests.rs` and
`src/custody_tests.rs`. Integration tests are in `tests/invariants.rs` (policy), `tests/transfers.rs`
(local transfer path) and `tests/custody.rs` (custody), and run against a near-workspaces sandbox.

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
| Dispatch: native `transfer`, plain `ft_transfer` with 1 yocto, never `ft_transfer_call`, callback args and gas | unit `lifecycle_tests::native_request_creates_a_transfer_then_a_private_callback`, `token_request_calls_plain_ft_transfer_with_one_yocto`; sandbox `transfers::token_transfer_calls_ft_transfer_and_commits_the_spend_once` (mock insists on 1 yocto and the recipient must receive via `ft_transfer`) |
| Exactly-once settlement: a successful transfer commits the amount once | unit `lifecycle_tests::successful_transfer_moves_the_reservation_into_the_window_once`; sandbox `transfers::native_transfer_pays_the_recipient_and_commits_the_spend_once`, `token_transfer_calls_ft_transfer_and_commits_the_spend_once` |
| Clean reversion: a failed transfer commits nothing and the budget returns | unit `lifecycle_tests::failed_transfer_releases_the_reservation_and_commits_nothing`; sandbox `transfers::failed_native_transfer_restores_the_budget` (nonexistent recipient), `failed_token_transfer_restores_the_budget` (mock `ft_transfer` panics) |
| Callback never panics, and never over-releases | unit `lifecycle_tests::settlement_never_panics_even_if_it_releases_more_than_is_reserved`, `settlement_is_independent_of_policy_changes_made_in_flight` |
| Ambiguous success still counts as spent | unit `lifecycle_tests::an_unexpected_success_payload_still_counts_as_spent` |
| No stranded reservation: a denied or reverted request leaves `reserved` unchanged | unit `lifecycle_tests::a_denied_request_reserves_nothing`; sandbox `transfers::denied_request_reverts_and_reserves_nothing`, `request_without_gas_for_the_callback_reverts_and_reserves_nothing` |
| `owner_withdraw` is owner-only | unit `custody_tests::only_the_owner_can_withdraw`; sandbox `custody::only_the_owner_can_withdraw` (agent and outsider revert, payee unchanged) |
| Owner recovery in any status: works while `Frozen` and with the mandate revoked | unit `custody_tests::owner_withdraw_works_while_frozen_and_after_revocation`; sandbox `custody::owner_recovers_funds_while_frozen` (agent is `Frozen`, owner's funds reach `to`) |
| `owner_withdraw` is outside policy: no caps, no allowlist, no policy needed | unit `custody_tests::owner_withdraw_ignores_policy`; sandbox `custody::native_withdraw_pays_the_recipient_without_regard_to_policy`, `token_withdraw_pays_the_recipient_without_regard_to_policy` |
| `owner_withdraw` does not touch `reserved` or the window | unit `custody_tests::owner_withdraw_does_not_touch_accounting`; sandbox `custody::owner_withdraw_does_not_touch_reservations_or_the_window` (withdraws mid-flight; accounting must stay one of the two consistent states), `failed_withdrawal_fails_the_promise_and_changes_no_accounting` |
| Native and NEP-141 withdrawals both pay `to`, plain `ft_transfer` with 1 yocto, no callback | unit `custody_tests::native_withdraw_pays_the_recipient_and_attaches_no_callback`, `token_withdraw_is_a_plain_ft_transfer_with_one_yocto_and_no_callback`; sandbox `custody::native_withdraw_pays_the_recipient_without_regard_to_policy`, `token_withdraw_pays_the_recipient_without_regard_to_policy` |
| Native `deposit()` raises the contract balance; zero deposit reverts | sandbox `custody::native_deposit_increases_the_contract_balance`, `deposit_without_an_attached_amount_reverts`; unit `custody_tests::deposit_accepts_an_attached_amount_from_anyone`, `deposit_without_an_attached_amount_reverts` |
| `ft_on_transfer` accepts a token with a policy (refund 0) | unit `custody_tests::ft_on_transfer_accepts_an_allowlisted_token_in_full`; sandbox `custody::ft_on_transfer_accepts_a_token_that_has_a_policy` |
| `ft_on_transfer` refunds any other token in full, and keys on the caller, not the claimed sender | unit `custody_tests::ft_on_transfer_refunds_a_token_without_a_policy_in_full`, `ft_on_transfer_keys_on_the_calling_token_not_the_claimed_sender`; sandbox `custody::ft_on_transfer_refunds_a_token_without_a_policy_in_full` |
| `ft_on_transfer` changes no accounting | unit `custody_tests::ft_on_transfer_does_not_change_accounting`; sandbox `custody::ft_on_transfer_leaves_accounting_alone` |
| EVM encoding: unsigned bytes, signing hash and signed bytes match viem byte for byte | unit `evm::unsigned_encoding_matches_viem`, `signing_hash_matches_viem`, `signed_encoding_matches_viem` (all six vectors) |
| Minimal-length integers and RLP header boundaries | the `zero_fields`, `single_byte_boundaries`, `list_payload_55_bytes`, `list_payload_56_bytes` and `high_top_bytes` vectors; unit `evm::vector_set_covers_the_list_header_boundary_and_zero_integers`, `length_prefix_matches_the_rlp_specification` |
| Keccak is correct | unit `evm::keccak_matches_known_vectors`, plus the viem signing hashes |
| High `s` is flipped and parity toggled; low `s` passes through | unit `evm::high_s_is_flipped_and_parity_toggled`, `low_s_passes_through_unchanged` (including `n/2` itself and `n-1`), `normalizing_twice_changes_nothing_more` |
| Invalid signer output is rejected, not encoded | unit `evm::zero_s_is_rejected`, `s_at_or_above_the_order_is_rejected`, `recovery_id_other_than_0_or_1_is_rejected`, `encoding_rejects_a_y_parity_other_than_0_or_1` |
| Callback is private | sandbox `transfers::only_the_contract_itself_can_call_the_settlement_callback` (the `#[private]` guard lives in the generated entry point, so unit tests cannot reach it) |

Sandbox time does not track block counts precisely, so no integration test asserts an exact time
boundary.

## Running

```
cargo near build non-reproducible-wasm --locked   # production wasm, no test helper
cargo test                                        # unit + sandbox tests; builds the test wasms itself
```
