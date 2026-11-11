# Minder: design

Scope so far: pure policy state and evaluation, the local NEAR enforcement path (the agent moves
NEAR or NEP-141 tokens held by this contract, gated by policy, with a reserve/settle/release
lifecycle), fund custody (owner withdrawal and deposits), the pure EVM transaction encoder, and the
cross-chain signing gate (`request_evm_payment`): the agent pays native ETH on an EVM chain by getting
the Chain Signatures contract to sign a transaction the contract built itself. Access-key mandates
and ERC-20 payments are not built yet.

## Layout

| File | Role |
| --- | --- |
| `src/types.rs` | `AssetId`, `Status`, `Config`, `AssetPolicy`, `Mandate`, `DenyReason`, `Decision` |
| `src/policy.rs` | `evaluate(&EvalInput) -> Decision`: pure, no state, no I/O. The only place policy rules live |
| `src/window.rs` | `SpendRing`: the sliding window. Pure; takes `now` and `slot_len` explicitly |
| `src/evm.rs` | Pure EIP-1559 transfer encoding: `Eip1559Tx`, `encode_unsigned`, `signing_hash`, `normalize_s`, `encode_signed`, and the gate-facing `try_assemble_signed`; hex and address helpers. No state |
| `src/gate.rs` | The EVM signing gate: owner EVM setters, `request_evm_payment`, `on_sign_complete`, EVM views |
| `vectors/gen-evm-vectors.mjs`, `vectors/gen-gate-vectors.mjs` | Generate the viem reference values that the `src/evm.rs` unit tests and `tests/evm_gate.rs` hard-code |
| `src/lib.rs` | Contract state, owner-only methods, `owner_withdraw`, `deposit`, `ft_on_transfer`, `request_local_transfer`, `on_transfer_complete`, views. Gathers state into `EvalInput` and calls `evaluate`. `transfer_promise` builds the transfer for both the agent path and owner withdrawal |
| `src/lifecycle_tests.rs`, `src/custody_tests.rs`, `src/gate_tests.rs` | Unit tests under the mocked environment: reserve/dispatch/settlement, custody, and the EVM gate |
| `src/test_support.rs` | Fixtures shared by those unit tests |
| `../test-contracts/mock-ft` | Test double for a NEP-141 token: records `ft_transfer`, can fail or stretch it over blocks, and `ft_transfer_call` drives a receiver's `ft_on_transfer`. Never deployed |
| `../test-contracts/mock-signer` | Test double for Chain Signatures: enforces the real `sign` preconditions (1 yocto, 15 Tgas, domain 0), records requests, and returns a fixed valid signature, a bad one, valid JSON of the wrong shape, a non-JSON blob, a genuine signature padded past 4096 bytes, or fails; can stretch the answer over blocks. Never deployed |

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
| `evm_config` | `Option<EvmConfig { signer_id, derivation_path, chain_id, max_fee_per_gas, max_priority_fee_per_gas, gas_limit }>` | Owner-set; `signer_id` and `derivation_path` fixed once set |
| `evm_recipients` | `IterableSet<[u8; 20]>` | Allowed EVM recipients, separate from `recipients` |
| `evm_nonces` | `LookupMap<u64, u64>` | Next nonce per chain id; only ever increases |

`AssetId` is `Near | Nep141(AccountId) | Evm(u64)`; `Evm(chain_id)` is native ETH on that chain. The policy, spend ring and reserved counter key on `AssetId` unchanged. The NEAR entry points (`request_local_transfer`, `dry_run`, `owner_withdraw`) refuse `Evm` assets, which are paid only through `request_evm_payment`.

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
and does no I/O. The gate calls `signing_hash` to hash its own transaction and `try_assemble_signed`
to turn the signer's response into a raw signed transaction.

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
| `try_assemble_signed(&tx, affine_point, scalar, recovery_id)` | `Result<Vec<u8>, EvmError>`: validates the signer's raw output, takes `r` from `affine_point[1..]`, normalizes `s`, and encodes. **Never panics** |

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
- **Invalid input panics in the low-level functions:** `normalize_s` rejects `recovery_id > 1` and `s`
  outside `1..n`; `encode_signed` rejects `y_parity > 1`. Better a loud failure than a transaction
  that recovers to another signer. The gate never reaches those asserts, because it goes through
  `try_assemble_signed`, which checks every precondition first and returns an `EvmError`
  (`RecoveryId`, `Scalar`, `AffinePoint` or `R`) instead. `r` is also required to be in `1..n`, which
  only an overflowing x coordinate would violate (those carry recovery id 2 or 3, already refused).
  The `02`/`03` prefix is not compared with `recovery_id`: the signer may have negated `s`.

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

## EVM signing gate (`src/gate.rs`)

`request_evm_payment(recipient, amount)` is the same reserve/settle/release lifecycle as the local
transfer, with a `sign` call to the Chain Signatures contract in place of the transfer. The spike
(`spikes/yield-resume-sign`) showed the signer's `sign` is itself a yield, so a plain
`sign(...).then(callback)` suspends until the MPC answers, and the signer's timeout arrives as
`Err(PromiseError::Failed)`. The gate uses exactly that; it creates no yield of its own.

### Flow

1. Read the EVM config, parse the recipient (`0x` + 40 hex digits), and evaluate with `evaluate` for
   `AssetId::Evm(chain_id)`: `effective_spend = ring.live_sum + reserved`, the recipient checked
   against the **EVM** allowlist. A denial reverts with `Denied: <Reason>`; nothing changes.
2. Reserve the **outlay** (below) in `reserved[Evm(chain_id)]`, before any promise.
3. Take the nonce: `evm_nonces[chain_id]` is read and incremented synchronously.
4. Build the `Eip1559Tx` from owner config and the taken nonce (`to` and `value` are the only agent
   inputs) and compute `evm::signing_hash`.
5. Call `signer.sign({ request: { path, payload_v2: { Ecdsa: <hash hex> }, domain_id: 0 } })` with
   1 yoctoNEAR and 30 Tgas (the signer needs 15), then `.then` the private
   `on_sign_complete(asset, outlay, tx)` with 25 Tgas reserved for it. Its arguments are borsh.

`on_sign_complete` never panics. In order:

1. Release the reservation (saturating, unconditional).
2. Read the result with `promise_result_checked` and parse the JSON ourselves (never
   `#[callback_result]`). The helper separates the cases: `Err(Failed)` means the signer's receipt
   failed (its own timeout included); `Err(TooLong)` means the receipt **succeeded** but its output
   exceeds our 4096-byte read buffer; `Ok(bytes)` is a readable output. `PromiseError` is
   non-exhaustive, so a variant added later is treated as "not proven failed".
3. **`Err(Failed)`: nothing was signed.** Log it and stop. Nothing is booked.
4. **Anything else: the receipt did not fail, so book the outlay (`record_spend`) unconditionally.**
5. Only then try to use the output. If it was readable and `try_assemble_signed` returns a raw
   transaction, log the NEP-297 event
   `EVENT_JSON:{"standard":"minder","version":"1.0.0","event":"evm_payment_signed","data":[{chain_id,
   nonce, to, value, raw_tx}]}` for an off-chain relayer to broadcast. Otherwise (unreadable,
   unparseable, or refused by the assembler) log `evm payment booked, no transaction emitted: <why>`
   and emit nothing.

**Principle (G1 fix): a successful signer receipt books spend regardless of whether the contract could
parse, size-fit or emit the signature, because that signature is public and broadcastable.** The
signer's response sits in a public receipt (the agent can read it from its own transaction's
outcome), so a signature this contract fails to read is still a signature anyone can assemble and
broadcast. Booking only after our own assembly succeeded left exactly that spend unaccounted; a
response-format change by the signer, or a response past the read buffer, was enough to trigger it.
Booking keys on the receipt, never on our ability to read it. This matches the local path
(`on_transfer_complete` books unless the transfer definitely failed). The cost is deliberate
over-counting: a signature we refuse (for instance recovery id 2, which cannot sign an Ethereum
transaction) still uses budget for one window.

### EVM policy surface

- **Policy, window and reservations** are the existing per-asset structures, keyed by `Evm(chain_id)`.
  Caps and limits for an EVM asset are in wei.
- **`evm_recipients`** is a set of 20-byte addresses, distinct from the NEAR recipient set, managed
  by `set_evm_recipient(address, allowed)`. Addresses are `0x` + 40 hex digits; an EIP-55 checksum
  is not verified.
- **`set_evm_config(signer_id, derivation_path, chain_id, max_fee_per_gas, max_priority_fee_per_gas,
  gas_limit)`** (`gas_limit` defaults to 21000). It rejects a zero chain id or gas limit, an empty
  path, a tip above the fee, and a fee times gas that overflows.
- **Views:** `dry_run_evm(caller, recipient, amount)` goes through the same evaluation as the real
  path; `get_evm_config`, `get_evm_nonce(chain_id)`, `is_evm_recipient_allowed`, `get_evm_recipients`.

### Hash-integrity guarantee

No method signs a caller-supplied hash. The only value ever sent as `payload_v2` is
`evm::signing_hash` of a transaction the contract built in the same call from the fixed
`chain_id`, `max_fee_per_gas`, `max_priority_fee_per_gas`, `gas_limit` (owner), the nonce (contract)
and the validated `to` and `value` (agent). Policy was evaluated on exactly those `to` and `value`
in the same receipt. The callback carries the same `Eip1559Tx` back by value, so the transaction it
assembles is the one that was hashed. The sandbox tests compare the hash the mock signer receives
with viem's keccak of viem's serialization.

### Owner-controlled gas, contract-owned nonce

- **The agent supplies neither gas nor nonce.** Fees and gas limit are fixed by the owner. A
  caller-chosen fee could burn funds; a caller-chosen nonce could replay or skip.
- **The contract owns the nonce.** One counter per chain id, incremented synchronously at request
  time, so two requests can never share a nonce. It cannot be lowered. `raise_evm_nonce(chain_id,
  nonce)` lets the owner raise it for an address that already has history on that chain (the
  capstone address had nonce 1); raising can open a gap but can never reissue a nonce.
- **`signer_id` and `derivation_path` are write-once.** Together they fix the sending address, and
  the nonce counter is only valid for one address per chain. Changing either would leave the counter
  describing a different account. Chain id and fees may change.

### Outlay: fees count against policy

Policy counts the **outlay**, `value + gas_limit * max_fee_per_gas`, not just the value, for the
per-transaction cap, the window limit, the reservation and the committed spend. The fee leaves the
same address as the value; if only the value were counted, an agent could send many tiny payments,
each within every cap, and drain the address through fees. The worst-case fee is used, so the
accounting over-counts (the actual fee is the base fee plus tip, usually far below `max_fee`), which
errs on the side of denying. Owners should size caps and limits to include it. This departs from the
literal "reserve `amount`" and is the one place the gate counts something other than the requested
value.

### Nonce gaps and other limits

- **A failed signing leaves a nonce gap.** The nonce was taken at request time. If the signer fails or
  times out, or the output is unusable, the reservation is released but that nonce is consumed; on
  the destination chain, later transactions from the address queue behind the missing one until the
  owner fills it (for instance with a self-transfer at that nonce signed outside Minder) or they are
  never mined. This is the accepted outbound-only liveness limit and is not reclaimed here.
- **Unusable signer output is booked, not ignored.** If the signer's receipt succeeds but the
  output cannot be turned into a transaction (wrong shape, not JSON, past the read buffer, or refused
  by `try_assemble_signed`), the outlay is still booked and no event is emitted. The signer is
  trusted, so this should not occur; booking makes the failure safe if it does. Only a failed receipt
  books nothing.
- **The signed transaction is public** (it is in the event log, and the signer's own response is in
  its receipt). Anyone can broadcast it; broadcasting is not a policy decision. It can be broadcast
  only once, by nonce.
- **The contract does not verify the signature** against the derived public key. It trusts the
  configured signer. An `ecrecover` check would be a hardening step.
- **Gas.** On testnet the whole request burnt about 14 Tgas, of which `on_sign_complete` used 2.1 of
  its 25 reserved. The request needs about 55 Tgas prepaid plus its own work (`sign` 30 + callback 25);
  with less, promise creation fails and the whole request reverts with nothing reserved.

### Testnet capstone

Production wasm deployed to `minder-spike-d5614e6c.testnet` (the account whose derived address for
path `ethereum-1` was funded by the Sepolia spike), owner and agent as separate faucet-funded
accounts, signer `v1.signer-prod.testnet`. Configuration: `Evm(11155111)` policy (cap 2e14 wei,
limit 1e15), recipient `0x…dEaD`, `max_fee_per_gas` 1 gwei, tip 100 wei, `raise_evm_nonce` to 1.
The agent's `request_evm_payment(…dEaD, 1e14)` returned in about 6 s of chain time (10 blocks), the
contract emitted the signed transaction for nonce 1, and the Sepolia harness recovered its signer as
the derived address `0xf18A6Cb7cC02156604b06025F2342976eeeA6083`, broadcast it, and it confirmed:
Sepolia tx `0x1427c99004f0f8632aedd670f9ad457633841c4a90464c017f4ed051572fda63`, block 12089295,
status success. Afterwards `reserved` was 0, the committed spend 1.21e14 (value plus the 2.1e13
worst-case fee), and the next nonce 2.

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
  without the feature omits it from the wasm exports and the ABI (its own gated impl block; checked).
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
- **Fees are counted in the EVM outlay** (see the gate section); this is the main departure from
  "reserve `amount`" and a one-line change to revert if you disagree.
- **EVM config is one chain at a time,** with a nonce counter per chain id. The signer and path are
  write-once; switching chains reuses the same sending address and its own counter.
- **The gate has its own owner-only `raise_evm_nonce`,** not asked for, because an address with
  history needs its counter started above zero.
- **`EvmConfig.signer_id` is configuration, not a constant,** so the same wasm runs against the real
  signer on testnet and a mock in the sandbox.
- **`EVM assets are refused by the NEAR entry points** rather than treated as an asset with a NEAR
  recipient, so a policy for `Evm(…)` can only ever be spent through the gate.
- **`EvalInput.live_spend` was renamed `effective_spend`** to say what it now holds.
- **`owner_withdraw` shares `transfer_promise` with the agent path** so there is one place that
  builds a transfer and "plain `ft_transfer`, never `ft_transfer_call`" is enforced once.
- **`ft_on_transfer` decides on policy existence only.** It does not check the amount, the sender or
  the status; a frozen contract still accepts allowlisted tokens, since funding is not agent spend.
- **The mock token has `ft_transfer_call`** only to exercise `ft_on_transfer`. The dispatch tests
  still show the agent path never uses it: the unit tests inspect the receipts, and the sandbox tests
  require the recipient to receive tokens through `ft_transfer`.

## Invariant to test map

Unit tests are in `src/window.rs`, `src/policy.rs`, `src/evm.rs`, `src/gate.rs`,
`src/lifecycle_tests.rs`, `src/custody_tests.rs` and `src/gate_tests.rs`. Integration tests are in
`tests/invariants.rs` (policy), `tests/transfers.rs` (local transfer path), `tests/custody.rs`
(custody) and `tests/evm_gate.rs` (the gate), and run against a near-workspaces sandbox.

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
| Gate: a request reserves the outlay (value plus worst-case fee) and takes the next nonce | unit `gate_tests::a_request_reserves_the_outlay_and_takes_the_next_nonce`, `each_request_gets_its_own_nonce_and_hash`; sandbox `evm_gate::a_returned_signature_settles_once_and_emits_the_signed_transaction` |
| Hash integrity: the signer is sent a hash the contract computed from owner config, its nonce and the validated recipient and value | unit `gate_tests::the_contract_sends_the_signer_a_hash_it_computed_itself`; sandbox `evm_gate::the_signer_is_sent_the_hash_the_contract_computed` (viem's hash for nonces 0 and 1, predecessor is the contract, configured path, domain 0) |
| The callback carries exactly the hashed transaction | unit `gate_tests::the_contract_sends_the_signer_a_hash_it_computed_itself` (borsh args equal asset, outlay, tx) |
| Settlement: a returned signature commits the spend once and emits the signed transaction | unit `gate_tests::a_returned_signature_settles_once_and_emits_the_signed_transaction`, `a_high_s_signature_is_normalized_before_the_transaction_is_emitted`; sandbox `evm_gate::a_returned_signature_settles_once_and_emits_the_signed_transaction` (event `raw_tx` equals viem's) |
| Clean reversion: a failed signer receipt (timeout or error) releases and books nothing; the nonce stays consumed | unit `gate_tests::a_failed_signer_call_releases_and_commits_nothing`; sandbox `evm_gate::a_failing_signer_releases_the_reservation_and_settles_nothing` |
| G1: a successful signer receipt books the outlay even if the output cannot be read, parsed or assembled; nothing is emitted | unit `gate_tests::a_successful_receipt_we_cannot_use_is_booked_but_emits_nothing` (eleven cases: recovery id 2, zero s, s at the order, bad prefix, short point, not hex, wrong shape, not JSON, empty, oversized garbage, oversized *valid* signature), `an_unreadable_but_successful_result_is_logged_as_unreadable`, `booked_unusable_output_counts_against_the_window`; sandbox `evm_gate::a_signature_that_cannot_sign_a_transaction_is_booked_but_not_emitted`, `valid_json_of_the_wrong_shape_is_booked_but_not_emitted`, `a_non_json_result_is_booked_but_not_emitted`, `a_genuine_signature_beyond_the_read_buffer_is_booked_but_not_emitted` |
| Only a failed receipt books nothing | unit `gate_tests::only_a_failed_receipt_books_nothing`, `a_failed_signer_call_releases_and_commits_nothing`; sandbox `evm_gate::a_failing_signer_releases_the_reservation_and_settles_nothing` |
| Callback never panics, even over-releasing | unit `gate_tests::settlement_never_panics_even_if_it_releases_more_than_is_reserved`, `settlement_is_independent_of_changes_made_in_flight`; `gate::tests::malformed_responses_are_rejected_without_panicking` |
| TOCTOU: a second request sees the first's reservation before its callback | unit `gate_tests::a_second_request_sees_the_first_ones_reservation`; sandbox `evm_gate::a_second_request_sees_the_first_ones_reservation_before_it_settles` (observes the reservation mid-flight, the denial, then both nonces settle) |
| Fees count against caps and the window; tiny payments cannot drain through fees | unit `gate_tests::fees_count_toward_the_per_transaction_cap`, `many_tiny_payments_cannot_drain_through_fees`; sandbox `evm_gate::denied_requests_change_nothing_and_never_reach_the_signer` |
| EVM allowlist is separate from the NEAR one; denials take no nonce and reach no signer | unit `gate_tests::the_evm_allowlist_is_separate_from_the_near_allowlist`, `a_denied_request_changes_nothing`; sandbox `evm_gate::denied_requests_change_nothing_and_never_reach_the_signer` |
| No stranded reservation or nonce on a reverted request | sandbox `evm_gate::a_request_without_gas_for_the_callback_reverts_and_reserves_nothing`, `evm_payments_need_configuration_and_a_valid_address` |
| EVM config is owner-only; identity is write-once; nonce only rises | unit `gate_tests::only_the_owner_sets_the_evm_config`, `only_the_owner_sets_evm_recipients`, `only_the_owner_raises_the_nonce`, `fees_and_chain_can_change_but_the_signing_identity_cannot`, `the_config_rejects_unusable_values`, `the_nonce_can_only_be_raised`; sandbox `evm_gate::evm_configuration_is_owner_only`, `the_signing_identity_is_fixed_but_fees_can_change`, `the_nonce_can_be_raised_but_never_lowered` |
| NEAR entry points refuse EVM assets | unit `gate_tests::the_local_entry_points_refuse_evm_assets`; sandbox `evm_gate::the_near_entry_points_refuse_evm_assets` |
| Signing callback is private | sandbox `evm_gate::only_the_contract_itself_can_call_the_signing_callback` |
| `try_assemble_signed` matches viem, normalizes high `s`, rejects every invalid input and never panics | unit `evm::assembling_a_valid_signature_matches_viem`, `assembling_a_high_s_signature_normalizes_it_to_the_viem_encoding`, `assembling_rejects_each_invalid_input`, `assembling_never_panics_on_any_combination_of_edge_inputs` |
| Signer response parsing and the event format | unit `gate::tests::a_well_formed_response_is_assembled`, `invalid_signatures_are_rejected_with_the_reason`, `the_event_carries_the_transaction_fields_and_raw_bytes` |
| End to end on testnet | the capstone above: real `v1.signer-prod.testnet`, real Sepolia confirmation (not an automated test) |
| Hex and address helpers | unit `evm::hex_round_trips_and_rejects_bad_input`, `addresses_parse_and_format` |
| Callback is private | sandbox `transfers::only_the_contract_itself_can_call_the_settlement_callback` (the `#[private]` guard lives in the generated entry point, so unit tests cannot reach it) |

Sandbox time does not track block counts precisely, so no integration test asserts an exact time
boundary.

## Model-based invariant harness (`src/model_tests/`)

A stateful, test-only harness that drives the real contract through long sequences of agent, owner,
callback and time actions and checks the contract against a shadow model after every step. The
example tests above pin individual scenarios; this harness checks the accounting and authority rules
across interleavings nobody wrote by hand. It is unit level (the mocked NEAR environment, no network),
deterministic, and compiled only for tests: `proptest` is a dev-dependency, and the production wasm
is unchanged.

**The shadow oracle is written independently of `policy.rs` and `window.rs`.** `model.rs` restates the
rules from the documentation: spend is a plain list of `(time, amount)` and the window is arithmetic on
slot numbers, so a bug in the contract's ring (aliasing, early forgetting) or in `policy::evaluate`
cannot be mirrored in the oracle. It also encodes the correct accounting: a reservation is released
exactly once, local spend is booked unless the transfer failed, and EVM spend is booked on any
successful signer receipt, valid or merely unusable (G1), with only a failed receipt booking nothing.

### How it works

- A `World` is the contract's persisted state (the mocked storage, including the borsh contract state)
  paired with the model. `World::apply(action)` restores that storage, runs one action against the real
  contract with the right caller, time and promise result, checks every invariant, and returns the next
  world.
- Allowed requests go on an **in-flight queue**. A `Deliver` action resolves the oldest or newest one
  later, with any result the token or signer can produce: for EVM a valid signature, a failed receipt
  (timeout), or a successful-but-unusable one (wrong-shape JSON, non-JSON garbage, a genuine signature
  padded past the read buffer); for local transfers success, failure, or success with an unexpected
  payload. This is what exercises reservations, TOCTOU and exactly-once settlement across orderings.
- Two assets (NEAR and EVM), an allowlisted and a non-allowlisted recipient, amounts at, below and
  over the per-transaction cap, owner tightening and loosening of a policy, grant, revoke, freeze,
  unfreeze, and time advances inside a slot, across a slot boundary and past a whole window.
- The exhaustive driver is a depth-first search that shares prefixes and **deduplicates states** by a
  fingerprint of the full stored state, the model, the clock and the queue. A repeated state is not
  expanded again, which is sound because the future is a function of that state. Actions that cannot
  change anything are skipped. So every state reachable within depth 8 is visited and every enabled
  action from it is executed and checked.
- The contract has no set of issued `(chain, nonce)` pairs, only the counter, so the harness records
  each pair from the transactions the contract builds and checks it is never issued twice.
- Every driver asserts **coverage**: it must actually have produced the kinds of step it claims to
  (allowed and denied requests by reason, every delivery outcome, and so on), so a driver cannot pass
  by silently doing nothing.

### State space explored (depth 8)

| Suite (test) | Actions | States | Transitions | Deduplicated | No-ops skipped | Focus |
| --- | --- | --- | --- | --- | --- | --- |
| accounting (`exhaustive_accounting_across_interleavings`) | 9 | 20,289 | 87,451 | 63,830 | 2,324 | requests at and below the cap that press the limit; deliveries of `Ok`, `Fail`, `WrongShape`, `Oversized` in either order; a window-length advance |
| authority (`exhaustive_authority_and_time`) | 10 | 30,715 | 127,486 | 87,389 | 29,194 | freeze, unfreeze, revoke, short and long grants, slot advances, interleaved with in-flight requests |
| policy (`exhaustive_policy_changes_and_caps`) | 11 | 13,486 | 73,721 | 54,642 | 11,749 | tightening and loosening limits under in-flight spend, over-cap and non-allowlisted requests, `Garbage` delivery |
| **Total** | | **64,490** | **288,658** | | | |

The random driver (`random_long_sequences`) runs 4,000 proptest cases of up to 40 actions over the
**full** alphabet from a fixed seed, so a build always draws the same cases; a failure is shrunk to a
minimal sequence. `MINDER_MODEL_CASES` raises the count for a longer run. The byte-level fuzz runs
10,000 cases per property from the same seed.

### Invariant to test map

"Yes" means that driver's alphabet actually reaches the invariant, not merely that the check runs on
every step. Every step of every harness driver runs every check.

| Invariant | Accounting | Authority | Policy | Random | Fuzz |
| --- | --- | --- | --- | --- | --- |
| Window spend equals the oracle after every step, including aging past slot and window boundaries (the ring never under-counts) | yes | yes | yes | yes | |
| Reserved equals the oracle and the sum of in-flight outlays; each reservation is released exactly once | yes | yes | yes | yes | |
| Reserved is zero once every callback has been delivered | yes | yes | yes | yes | |
| `live + reserved <= limit` for every asset, except while an owner-lowered limit is still above what was already counted | yes | yes | yes (lowers limits) | yes (lowers limits) | |
| Spend changes only on a delivery, and by exactly the delivered outlay (zero for a failed receipt) | yes | yes | yes | yes | |
| Allow or deny, and the exact deny reason, equal the oracle | window | frozen, revoked, expired | cap, recipient, window | all | |
| `dry_run` and `dry_run_evm` agree with the real path | yes | yes | yes | yes | |
| TOCTOU: in-flight reservations count against later requests | yes | yes | yes | yes | |
| Monotonic revocation: once frozen, revoked or expired, nothing is approved until `unfreeze` or a new grant (stated by separate ghost bookkeeping, not by the oracle's `decide`) | | yes | | yes | |
| Settlement still happens, and books correctly, after freeze, revoke or expiry while in flight | | yes | | yes | |
| Mandate (epoch, revoked, expiry) and frozen state equal the oracle | | yes | | yes | |
| EVM nonce: the counter equals the oracle, strictly increases, no `(chain, nonce)` is reused, and a failed signing still consumes its nonce | yes | yes | yes | yes | |
| Dispatch integrity: an allowed request builds exactly the transaction policy approved (owner-set fees, contract nonce, validated `to` and `value`), the callback carries it, and the signer is sent that transaction's hash and the configured path; local callbacks carry the right asset and amount | yes | yes | yes | yes | |
| G1 booking rule: only a failed receipt books nothing; a successful-but-unusable receipt books the outlay | `WrongShape`, `Oversized`, `Fail` | `Ok`, `Fail` only | `Garbage` | all | |
| The signed-transaction event is emitted if and only if the signature is valid, and an unusable result is logged as booked with nothing emitted | yes | yes | yes | yes | |
| Callbacks never panic (a panic would strand a reservation) | yes | yes | yes | yes | |
| No unexpected panic anywhere; every denial carries its exact `Denied: <Reason>` message | yes | yes | yes | yes | |
| `assemble_from_response` and `try_assemble_signed` never panic on arbitrary, mutated or structured bytes, and always return a `Result` | | | | | yes |
| They accept exactly the acceptable signatures (recovery id 0 or 1, `0 < s < n`, a 33-byte point starting `02` or `03`, `0 < r < n`), judged by a rule restated independently in the test | | | | | yes |

The fuzz tests are in `gate::tests`: `assemble_from_response_never_panics_on_arbitrary_bytes`,
`assemble_from_response_never_panics_on_mutated_real_signatures`,
`assemble_from_response_accepts_exactly_the_acceptable_signatures` and
`try_assemble_signed_never_panics_and_accepts_exactly_the_acceptable_signatures`. The two "accepts
exactly" tests assert they reached both accepted and rejected inputs in volume, so they are not vacuous.

### What it caught when the fix was reverted

**The G1 under-count regression is caught by the accounting and policy suites** (they fail with the
contract's window spend below the model's after a `WrongShape`, `Oversized` or `Garbage` delivery). The
random driver also catches it and shrinks it to two actions, a mid-sized EVM request followed by a
`WrongShape` delivery. The authority suite still passes, as expected: its alphabet has no
successful-but-unusable outcome. This was checked by temporarily reverting the fix in
`on_sign_complete`, running the harness, and restoring the file byte for byte.

Run it with `cargo test --lib model_tests` (about a minute and a half) and the fuzz with
`cargo test --lib gate::tests`.

## Running

```
cargo near build non-reproducible-wasm --locked   # production wasm, no test helper
cargo test                                        # unit + sandbox tests; builds the test wasms itself
```
