# Minder: design

Scope so far: pure policy state and evaluation, the local NEAR enforcement path (the agent moves
NEAR or NEP-141 tokens held by this contract, gated by policy, with a reserve/settle/release
lifecycle), fund custody (owner withdrawal and deposits), the pure EVM transaction encoder, and the
cross-chain signing gate (`request_evm_payment`, `request_evm_token_payment`): the agent pays native
ETH or an ERC-20 token on an EVM chain by getting the Chain Signatures contract to sign a transaction
the contract built itself, calldata included. The agent is a native function-call access key on the
contract's own account (see Agent authority).

## Layout

| File | Role |
| --- | --- |
| `src/types.rs` | `AssetId`, `Status`, `Config`, `AssetPolicy`, `Mandate`, `DenyReason`, `Decision` |
| `src/policy.rs` | `evaluate(&EvalInput) -> Decision`: pure, no state, no I/O. The only place policy rules live |
| `src/window.rs` | `SpendRing`: the sliding window. Pure; takes `now` and `slot_len` explicitly |
| `src/evm.rs` | Pure EIP-1559 encoding (native transfers and ERC-20 `transfer` calls): `Eip1559Tx`, `erc20_transfer_calldata`, `encode_unsigned`, `signing_hash`, `normalize_s`, `encode_signed`, and the gate-facing `try_assemble_signed`; hex and address helpers. No state |
| `src/timelock.rs` | The asymmetric owner timelock: classification, `propose_change`, `execute_change`, `cancel_change`, `set_timelock_delay`, `set_treasury` |
| `src/decision.rs` | The decision log: the `decision` NEP-297 event every agent request emits, allowed or denied |
| `src/gate.rs` | The EVM signing gate: owner EVM setters, `request_evm_payment`, `request_evm_token_payment`, `on_sign_complete`, owner recovery (`owner_evm_withdraw`, `owner_evm_token_withdraw`, `on_owner_sign_complete`), EVM views |
| `vectors/gen-evm-vectors.mjs`, `vectors/gen-gate-vectors.mjs` | Generate the viem reference values that the `src/evm.rs` unit tests and `tests/evm_gate.rs` hard-code |
| `src/lib.rs` | Contract state, owner-only methods, `owner_withdraw`, `deposit`, `ft_on_transfer`, `request_local_transfer`, `on_transfer_complete`, views. Gathers state into `EvalInput` and calls `evaluate`. `transfer_promise` builds the transfer for both the agent path and owner withdrawal |
| `src/lifecycle_tests.rs`, `src/custody_tests.rs`, `src/gate_tests.rs`, `src/mandate_tests.rs` | Unit tests under the mocked environment: reserve/dispatch/settlement, custody, the EVM gate, and the agent key's provisioning, revocation and authentication |
| `src/test_support.rs` | Fixtures shared by those unit tests |
| `../test-contracts/mock-ft` | Test double for a NEP-141 token: records `ft_transfer`, can fail or stretch it over blocks, and `ft_transfer_call` drives a receiver's `ft_on_transfer`. Never deployed |
| `../test-contracts/mock-signer` | Test double for Chain Signatures: enforces the real `sign` preconditions (1 yocto, 15 Tgas, domain 0), records requests, and returns a fixed valid signature, a bad one, valid JSON of the wrong shape, a non-JSON blob, a genuine signature padded past 4096 bytes, or fails; can stretch the answer over blocks. Never deployed |

## State

| Field | Type | Notes |
| --- | --- | --- |
| `config` | `Config { owner, status, window_len, num_buckets, timelock_delay, near_treasury, evm_treasury }` | Set in `init`; `status`, the delay and the treasuries change later, the last two only under the timelock rule |
| `pending_changes`, `next_change_id` | `IterableMap<u64, PendingChange>`, `u64` | Loosening owner changes waiting out the delay; ids start at 1 and are never reused |
| `mandate` | `Option<Mandate { agent_key: PublicKey, expiry, epoch, revoked, scope: MandateScope }>` | One mandate at a time; `agent_key` is the function-call key provisioned on the contract account |
| `last_mandate_epoch` | `u64` | Incremented on every grant, never reset, so epochs are never reused |
| `policies` | `IterableMap<AssetId, AssetPolicy { per_tx_cap, window_limit, epoch_budget }>` | Having an entry allowlists the asset; `epoch_budget` is optional |
| `recipients` | `IterableSet<AccountId>` | Allowed recipients |
| `spend` | `LookupMap<AssetId, SpendRing>` | Per-asset committed window accounting |
| `epoch_spend` | `LookupMap<AssetId, EpochSpend { epoch, committed }>` | Per-asset committed spend under one mandate epoch, for the lifetime budget. A stale epoch reads as zero. Kept when a policy is removed |
| `reserved` | `LookupMap<AssetId, u128>` | Per-asset amount reserved for transfers still in flight. Absent means zero |
| `evm_config` | `Option<EvmConfig { signer_id, derivation_path, sender, chain_id, max_fee_per_gas, max_priority_fee_per_gas, gas_limit, token_gas_limit }>` | Owner-set; `signer_id`, `derivation_path` and `sender` fixed once set |
| `evm_recipients` | `IterableSet<[u8; 20]>` | Allowed EVM recipients, separate from `recipients` |
| `evm_nonces` | `LookupMap<u64, u64>` | Next nonce per chain id; only ever increases |

`AssetId` is `Near | Nep141(AccountId) | Evm(u64) | Evm20 { chain_id, token }`; `Evm(chain_id)` is native ETH on that chain and `Evm20` an ERC-20 token on it. The policy, spend ring and reserved counter key on `AssetId` unchanged. The NEAR entry points (`request_local_transfer`, `dry_run`, `owner_withdraw`) refuse `Evm` assets, which are paid only through `request_evm_payment`.

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

### Lifetime budget per mandate grant

A per-asset budget that bounds one grant however many windows elapse. How a spend is bounded
without it: the per-transaction cap, and the sliding window (`effective_spend + amount <= window_limit`).
Both are rates; a budget is the aggregate.

- **`AssetPolicy.epoch_budget: Option<U128>`**, set by the owner with
  `set_asset_policy(asset, per_tx_cap, window_limit, epoch_budget)`. `None` (or omitting it from the
  call) means no budget, only the window, so existing policies and calls are unaffected.
- **A per-asset counter `epoch_spend[asset] = (mandate_epoch, committed)`** is written by
  `record_spend` at the same point it records into the window ring: `(current epoch,
  committed-this-epoch + outlay)`. It is kept whether or not a budget is set, so a budget set
  mid-grant counts what the grant already spent (it is not forgiven), and, like the ring, it survives
  `remove_asset_policy`, so removing and re-adding a policy cannot reset a budget.
- **Lazy epoch reset.** `Minder::epoch_committed` reads the stored amount only if its epoch equals
  the current mandate's, and otherwise zero. A new grant bumps the mandate epoch, which resets every
  asset's budget to zero **without touching storage**; the stale entry is simply overwritten by the
  next settlement. Revoking does not change the epoch, so spend after a revoke (an in-flight
  settlement) still counts for the same grant. There is no forever-growing counter.
- **In-flight spend counts, through the shared reserved counter.** Budget usage is
  `epoch_committed + reserved[asset]` (`EvalInput.epoch_usage`, built with `policy::effective_spend`,
  the same function as for the window). A request reserves its outlay before dispatch, so a second
  request sees the first before it settles, and the budget is TOCTOU-safe by the mechanism the window
  already uses. A failed delivery releases the reservation and so gives the budget back.
- **`evaluate` denies `MandateBudgetExceeded`** when `epoch_usage + amount > epoch_budget` (or
  overflows). It is a global check, made after the window check and before the mandate's scope: where
  both the window and the budget would deny, the reason is the window, as it was before budgets.
  `DenyReason::MandateBudgetExceeded` is appended, so existing borsh tags are unchanged. `dry_run`
  and `dry_run_evm` run `evaluate` and so reflect it. For EVM the amount is the outlay (value plus
  worst-case fee), as for the caps.
- **Settlement under a later grant.** A request reserved under one grant that settles after a
  re-grant is booked to the new grant (record_spend uses the epoch current at settlement). While in
  flight it already counted against the new grant as reserved, so this only tightens the new grant;
  the old grant's own limit was enforced at request time, reserved included.
- **Lowering a budget below the usage** denies everything of that asset until the next grant (or a
  raised budget); nothing is clawed back, like a lowered window limit.
- **Worst-case loss per grant per asset** is `min(sum of the windows over the grant's lifetime,
  epoch_budget)`, further narrowed by the mandate's scope.

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

## Agent authority: a function-call key on the contract account

**Auth model, before and after.** Before, the agent was a NEAR account and `evaluate` compared
`predecessor_account_id` with `Mandate.agent`. Now the agent is a **public key**. `grant_mandate`
adds it to the contract's own account as a function-call access key, and a request is the agent's
only if the contract is both its own predecessor and the call was signed by exactly the mandate's key:

```
predecessor_account_id == current_account_id   AND   signer_account_pk == mandate.agent_key
```

`Minder::authenticated_key()` returns `signer_account_pk` when the first holds and `None` otherwise;
`evaluate` denies `NotAuthorized` unless that equals the mandate's key. The contract's own callbacks,
the deployer's key and any other key on the account carry a different key and never match.

**Topology.**

```
owner account (separate, init rejects owner == contract)   -- owner methods, own auth
contract account  minder.near
  full-access key ........ the deployer's; REMOVED after deploy + init (deployment step below)
  function-call key ...... the agent's; receiver = minder.near,
                            method_names = request_local_transfer, request_evm_payment, request_evm_token_payment,
                            allowance = 0.25 NEAR; added by grant_mandate, deleted by revoke_mandate
```

**Why the scoping is native.** The runtime enforces a function-call key before any contract code runs:
`method_names` is the fixed list `AGENT_METHODS` in `lib.rs` (never a parameter), the receiver is the
contract itself, no deposit can be attached, and the key cannot transfer, deploy, or add or delete
keys. The sandbox suite `tests/agent_key.rs` signs with the real key and shows the runtime refusing
(`MethodNameMismatch`) every other exported method, owner methods, views, callbacks and `init`
included, without any contract logic running; refusing a call to the signer directly
(`ReceiverMismatch`); and refusing transfers, key changes and deployment (`RequiresFullAccess`).

**Provisioning (`grant_mandate(agent_key, expiry)`).** Owner only. It adds the key with
`Promise::add_access_key_allowance`, records `agent_key` and `expiry` in the `Mandate` and takes a
fresh epoch. If a live (not revoked) mandate exists its key is deleted first, and re-granting that same
live key is refused (`revoke` first or use a new key): its deletion and re-addition would race. Each key
action is **its own promise, never one batch**: the actions of a receipt succeed or fail together, so a
deletion of a key that is already gone would otherwise also cancel the addition of the new one.

**Revocation (`revoke_mandate`).** Owner only; reverts with no mandate; a second revoke is a no-op.
Two layers, both applied in the call:

1. The `revoked` flag, checked by `evaluate` before the key. It is synchronous: from the next call on
   the agent is denied `MandateRevoked`, whether or not the deletion has landed and even if it fails.
2. A `Promise::delete_key` receipt, the protocol-level removal: once it lands the runtime refuses the
   key outright (`AccessKeyNotFound`).

The deletion is detached, so a failure (the key is already gone) is a failed receipt that does not
fail `revoke_mandate`, which has already returned. Revocation is expressed by the flag, not by bumping
the epoch: the epoch identifies grants, and `evaluate` checks the flag.

**Asynchrony.** The key is added and deleted in a later block than the call. Between a grant and the
add, nobody can authenticate as the agent, so the delay only withholds authority. Between a revoke and
the delete, the key still reaches the contract, and the flag refuses it. Tests:
`after_a_revoke_the_epoch_guard_denies_even_if_the_key_survived_the_delete` puts the key back after a
revoke and shows the contract still denies it.

**Expiry** is enforced by `evaluate` (`MandateExpired`) and does not remove the key. An expired key
stays on the account, harmless, until the next revoke or grant deletes it.

**Mandate scope (least privilege).** A mandate can be granted strictly less than the global policy.
`grant_mandate(agent_key, expiry, scope)` takes an optional `MandateScope`, stored in the `Mandate`:

| Field | Meaning when `Some` |
| --- | --- |
| `allowed_assets: Option<Vec<AssetId>>` | the request's asset must be listed |
| `allowed_near_recipients: Option<Vec<AccountId>>` | for NEAR and NEP-141 transfers, the recipient must be listed |
| `allowed_evm_recipients: Option<Vec<String>>` | for EVM payments, the recipient must be listed (`0x` addresses, entered in any case, stored lowercase) |
| `per_tx_cap: Option<U128>` | an extra per-transaction cap |

- **The effective authority is the intersection of the global policy and the scope; a scope only
  subtracts.** `evaluate` runs every global check first (status, mandate, key, the asset's policy,
  recipient allowlist, per-transaction cap, window). Only a request that passed all of them meets the
  scope, checked in this order: asset, recipient, then the cap. A scope can turn an allow into a
  deny, never the reverse, so an asset or recipient named in a scope but not in the global policy is
  still refused by the global check (`AssetNotAllowed`, `RecipientNotAllowed`).
- **Unset means inherit.** A `None` field adds no restriction, and `scope: None` (or omitting it from
  the JSON call) gives an all-`None` scope, which behaves exactly as a mandate did before scopes
  existed. `Some` of an empty list permits nothing of that kind. The NEAR and EVM recipient lists are
  separate, like the global allowlists, so scoping only one leaves the other surface at the global
  allowlist; scoping `allowed_assets` is what closes a whole surface.
- **The cap is the smaller of the two.** The effective cap is `min(global cap, mandate cap)`: the
  global cap is checked with the global checks, the mandate's afterwards, both reporting
  `PerTxCapExceeded`. A looser mandate cap never widens the global one. It is a single number in each
  asset's own units (for EVM, the outlay in wei), so it is only meaningful alongside an
  `allowed_assets` of assets that share a unit.
- **New deny reasons, distinct for audit** (like `NoMandate` and `MandateRevoked`): `AssetOutOfScope`
  and `RecipientOutOfScope`. They are appended to `DenyReason`, so the existing borsh tags are
  unchanged.
- **The window limit stays global** and per asset: a scoped mandate does not get a budget of its own,
  and this piece adds no lifetime budget.
- **`dry_run` and `dry_run_evm` evaluate the same scope**, because they run `evaluate` on the stored
  mandate, so a caller sees a scoped denial before sending.
- **A re-grant replaces the scope.** The new scope applies from the new grant; nothing carries over,
  and an unscoped re-grant drops it. Each list holds at most 64 entries (the work of one evaluation
  and the stored size), and an invalid EVM address in a scope reverts the grant.

**Gas allowance and worst-case loss.** The key's gas allowance is a fixed 0.25 NEAR (`AGENT_KEY_ALLOWANCE`,
a constant). It is spendable from the contract's own balance, so **the most the agent can lose the
owner is the policy limits plus this allowance per granted key**, and for a scoped mandate the policy
part is bounded by the tighter of the two: only the scope's assets, only to the scope's recipients,
at most `min(global cap, mandate cap)` per transaction, no more than the global window limit per
asset in any window, and no more than the asset's `epoch_budget` over the whole grant (a token
payment is bounded twice: the amount by the token's policy and its gas by the native asset's) (so
`min(sum of the windows over the grant, epoch_budget)` per asset). The allowance bounds the transaction
fees the key can burn, whatever it calls (denied requests burn gas too). The runtime checks the
allowance against the *prepaid* gas at the current gas price and gives unused gas back, so the agent
must attach what a request needs, not the 300 Tgas maximum; at the mainnet floor price (1e8 yoctoNEAR
per gas) 0.25 NEAR covers eight 300 Tgas calls, and the sandbox price is ten times that. The allowance
does not reset: re-issuing the key (revoke, then grant) is what restores it.

**Deployment (required).** The containment claim holds only if no full-access key remains on the
contract account. The contract does not delete its own key from a method; this is a deployment step:

1. Deploy the wasm to a fresh account and call `init` (in one batched transaction, with the
   deployer's key). `init` rejects an owner equal to the contract account, so the owner is always a
   separate account.
2. Remove the deployer's full-access key: `near account delete-keys <contract> --public-keys <key>`
   (or a `DeleteKey` action signed by that key).
3. Verify: `near account list-keys <contract>` shows no full-access key, and after the first grant
   exactly one function-call key with the receiver, methods and allowance above.

near-sdk exposes no host function to read an account's keys, so there is no on-chain view or assertion
for this; step 3 is manual. `tests/agent_key.rs` demonstrates both sides: with the deployer key still
on the account, its holder passes the `#[private]` guard of the callbacks (the guard only checks the
predecessor), and after the removal the account holds only the agent's key while the owner keeps full
control. Removing the key means the contract can no longer be redeployed or upgraded; the owner's
recovery path is `owner_withdraw`.

**Residual risks.** (a) If the owner grants a key that already exists on the account, such as the
deployer's own, the addition fails silently and that key is the agent: the owner supplies a fresh key.
(b) The agent key can burn its allowance on denied calls. (c) The runtime checks the allowance against prepaid gas at the current price (above), so a request attached with too much gas is refused before it runs.

**State schema.** `Mandate` changed twice: `agent: AccountId` became `agent_key: PublicKey`, and it
gained `scope: MandateScope`. Both change the borsh layout of the stored state, and `grant_mandate`
and `dry_run` take a key where they took an account (and `grant_mandate` an optional scope). No
migration is written: a testnet deployment from before these changes needs a fresh account or wiped
state. `AssetPolicy` (a trailing `epoch_budget`) and the contract state (a new `epoch_spend` map)
changed the same way, and so did `AssetId` (the appended `Evm20`), `EvmConfig` (a trailing
`token_gas_limit`) and `Eip1559Tx` (a trailing `data`, which is also the borsh form of the
`on_sign_complete` arguments, now `(Vec<(AssetId, u128)>, Eip1559Tx)`), and `set_asset_policy` takes an optional `epoch_budget`; a call without it still
works. The JSON of an existing `grant_mandate` call with a key still works unchanged (no `scope`
means unscoped).

## Owner timelock (asymmetric)

How an owner change took effect before: at once, whatever it did. Now the contract classifies each
change by what it does to authority: **tightening and emergency actions are instant, loosening ones
wait a configurable delay, and owner fund recovery is instant only to a pre-registered treasury.**
Defending a single owner key is still primarily the job of an external multisig owner; this adds the
in-contract reaction window and the safe-recovery constraint.

**The delay and the treasuries** are set at `init` (`timelock_delay` in ns, `near_treasury`,
`evm_treasury`, all optional) and change later only under the rules below. **A delay of zero (the
default) disables the timelock**: every change is instant, as before. That is how a deployment is
armed (adding assets and recipients is loosening), after which `set_timelock_delay` switches it on
(raising the delay is instant). Deploy, arm, then enable; a deployment that never enables it has no
timelock.

**Classification** (applied by the contract to the same methods as before, not by separate methods;
`timelock.rs::is_loosening`):

| Change | Instant when | Timelocked when |
| --- | --- | --- |
| `freeze`, `revoke_mandate`, `remove_asset_policy` | always | never |
| `set_asset_policy` | tighter or equal in every field (cap, window, budget; a budget where there was none counts as tighter) | adding the asset; a higher cap or window; dropping or raising the budget. A mix of raising one field and lowering another is loosening |
| `set_recipient`, `set_evm_recipient` | removing, or already as asked | adding |
| `grant_mandate` | no live mandate (none, revoked, expired), or the expiry does not extend the live one's | extending the expiry of a live mandate |
| `unfreeze` | not frozen | frozen |
| `set_evm_config` | tighter or equal: no higher fee or gas limit, same chain | the first configuration; a higher `max_fee_per_gas`, `max_priority_fee_per_gas`, `gas_limit` or `token_gas_limit`; another chain |
| `set_timelock_delay` | raising or equal | lowering |
| `set_treasury` | no change | any change |
| `owner_withdraw` | `to` is the NEAR treasury | any other destination |
| `owner_evm_withdraw`, `owner_evm_token_withdraw` | the recipient is the EVM treasury or the contract's own derived address | any other recipient |

Instant grants rest on the policy: an agent holding a malicious grant is still bounded by the current,
un-loosened caps, windows, allowlists and budgets, which the owner key cannot raise instantly. A
transfer to the derived address itself moves nothing out; it is how a stuck or signed-but-unbroadcast
transaction is cancelled, which must stay instant. `raise_evm_nonce` is not classified (it moves no
authority).

**Two-phase mechanism.** A method called directly with a loosening change reverts with `Timelocked`.
The owner instead calls `propose_change(change)` with the change as an `OwnerChange` (one variant per
loosening method), which validates it, records it with `ready_at = now + delay` under a fresh id and
returns the id (it refuses a change that is not timelocked: call the method). `execute_change(id)`
applies it once `now >= ready_at` and reverts with `Not ready` before; if applying it fails (a grant
whose expiry has since passed) the call reverts and the proposal stays, to be cancelled.
`cancel_change(id)` drops a proposal and is always available. All three are owner-only. Up to 32
proposals may be pending, and each proposal, execution and cancellation is logged as a NEP-297 event
(`owner_change_proposed`, `owner_change_executed`, `owner_change_cancelled`) for the control panel.
`get_pending_change(s)` list what is waiting.

**Tightening is never blocked.** A freeze, revocation or cap-lowering works at once even while a
loosening of the same thing is pending. The pending loosening is not touched by it: it stays until it is
executed or cancelled, so the owner who tightens in an emergency should also cancel what is pending.

**Treasury-constrained recovery.** Recovery to the registered treasury of its surface is instant; to
any other destination it is a timelocked proposal, executed (and, for EVM, signed) only after the
delay. With no treasury registered, all recovery is timelocked. Changing the treasury is itself
timelocked. A signing recovery executed through `execute_change` still passes the `ecrecover`
authenticity gate; the signing dispatch is in `apply_owner_evm_withdraw` and
`apply_owner_evm_token_withdraw`, which `execute_change` reaches only after an owner check, and which
are the entries on the signing-confinement list.

**What a compromised owner key can still do.** Queue changes (visible, and cancellable by whoever
else holds the key); freeze or revoke (denial of service); remove assets and recipients; lower caps;
instantly grant a mandate to its own key (bounded by the existing policy and allowlists: it cannot add
a recipient or raise a cap); send funds to the registered treasury, which it cannot change instantly.
Known limits: (a) revoke is instant and a grant after a revoke is instant, so the extension rule
(a timelock on lengthening a live mandate) can be walked around by revoking first, at the cost of
dropping the agent's authority visibly; (b) a grant starts a new epoch, which resets each asset's
lifetime budget, so instant re-grants refresh the budget (the windows still bound the rate); (c) the
delay is only a reaction window if someone is watching and holds the means to cancel: with one key,
the attacker holds it too, which is why the external multisig is the primary mitigation.

**State schema.** `Config` gained the delay and the treasuries and the contract gained the pending
map and the id counter, so a deployment from before this change needs a fresh account or wiped state.
`init` takes three more optional arguments (a call without them still works); the existing owner
methods keep their names, kinds and arguments; five calls and two views are new.

## Fund custody

This contract holds the funds the agent can spend: NEAR in its own balance, and NEP-141 tokens in
its account at each token contract. It keeps no ledger of them; the balances are the chain's.

### Threat-model boundary

**Minder contains the untrusted agent, not the owner.** The owner owns the treasury, so
`owner_withdraw` (and its cross-chain counterparts `owner_evm_withdraw` and `owner_evm_token_withdraw`)
is outside policy. A **single compromised owner key** is a separate threat. The primary mitigation is
external and not built here: **the owner should be a NEAR multisig account**, so no one key can act.
Inside the contract, the asymmetric timelock (next section) adds a reaction window and a
safe-recovery constraint on top: a compromised key cannot instantly widen the agent's authority or
send funds anywhere but the pre-registered treasury. It does not make a single-key owner safe: see
the limits listed there.

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
gas_limit: u64, to: [u8; 20], value: u128, data: Vec<u8> }`. The **access list is always empty**. For a
native ETH transfer `data` is empty; for an ERC-20 transfer `to` is the token, `value` is zero and
`data` is `erc20_transfer_calldata(recipient, amount)`: the `0xa9059cbb` selector, the recipient
left-padded to 32 bytes, and the amount as a 32-byte big-endian word (68 bytes in all; the amount is a
`u128`, so the high 16 bytes of its word are zero). That is the only calldata the contract ever
builds. Widths follow the Ethereum fields: `u128` holds any wei value, fee or token amount that fits
in practice, `u64` holds nonce, gas and chain id.

### Functions

| Function | Result |
| --- | --- |
| `encode_unsigned(&tx)` | `0x02 \|\| rlp([chain_id, nonce, max_priority_fee_per_gas, max_fee_per_gas, gas_limit, to, value, data, access_list])` with the transaction's data and an empty access list |
| `erc20_transfer_calldata(&recipient, amount)` | the 68 bytes of `transfer(recipient, amount)` |
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

Three more vectors cover calldata: `erc20_transfer` and `erc20_transfer_max_amount` (the data is
viem's own `encodeFunctionData` of `transfer`, an encoder independent of ours), and the RLP string
edges `data_one_byte_below_0x80` (a single byte under `0x80` is its own encoding), `data_one_byte_0x80`,
`data_55_bytes` and `data_56_bytes` (the short and long string headers).

The seeded differential corpus (`vectors/gen-evm-corpus.mjs`, 1,743 cases) covers calldata too:
300 random and 40 edge ERC-20 transfers (amounts across every width boundary up to `u128::MAX`,
recipients at the address extremes) and data lengths from 0 to 1,000 bytes with the single-byte
cases. For every token case the test also rebuilds the calldata with `erc20_transfer_calldata` from
the recipient and amount viem encoded and requires viem's bytes back, so the contract's builder is
checked against viem's ABI encoder 340 times as well as the serialization and the hash.

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

(This is the native path, `request_evm_payment`. A token payment is the section after the
authenticity check.)

1. Read the EVM config, parse the recipient (`0x` + 40 hex digits), and evaluate with `evaluate` for
   `AssetId::Evm(chain_id)`: `effective_spend = ring.live_sum + reserved`, the recipient checked
   against the **EVM** allowlist. A denial reverts with `Denied: <Reason>`; nothing changes.
2. Reserve the **outlay** (below) in `reserved[Evm(chain_id)]`, before any promise.
3. Take the nonce: `evm_nonces[chain_id]` is read and incremented synchronously.
4. Build the `Eip1559Tx` from owner config and the taken nonce (`to` and `value` are the only agent
   inputs) and compute `evm::signing_hash`.
5. Call `signer.sign({ request: { path, payload_v2: { Ecdsa: <hash hex> }, domain_id: 0 } })` with
   1 yoctoNEAR and 30 Tgas (the signer needs 15), then `.then` the private
   `on_sign_complete(reservations, tx)` with 25 Tgas reserved for it, where `reservations` is the list
   of `(asset, amount)` pairs the request reserved (here the single `(Evm(chain_id), outlay)`). Its
   arguments are borsh.

`on_sign_complete` never panics. In order:

1. Release every reservation in `reservations`, each exactly once (saturating, unconditional).
2. Read the result with `promise_result_checked` and parse the JSON ourselves (never
   `#[callback_result]`). The helper separates the cases: `Err(Failed)` means the signer's receipt
   failed (its own timeout included); `Err(TooLong)` means the receipt **succeeded** but its output
   exceeds our 4096-byte read buffer; `Ok(bytes)` is a readable output. `PromiseError` is
   non-exhaustive, so a variant added later is treated as "not proven failed".
3. **`Err(Failed)`: nothing was signed.** Log it and stop. Nothing is booked.
4. **Anything else: the receipt did not fail, so book every reserved amount (`record_spend`) unconditionally.**
5. Only then try to use the output. If it was readable, the signature normalizes (`normalized_signature`:
   recovery id 0 or 1, `0 < r, s < n`, low `s`), and **it recovers to the configured `sender`**,
   log the NEP-297 event
   `EVENT_JSON:{"standard":"minder","version":"1.0.0","event":"evm_payment_signed","data":[{chain_id,
   nonce, to, value, raw_tx}]}` for an off-chain relayer to broadcast. Otherwise (unreadable,
   unparseable, or refused by the assembler) log `evm payment booked, no transaction emitted: <why>`
   and emit nothing. If the signature assembles but does not recover to `sender`, or recovers to
   nothing, log `evm payment booked, no transaction emitted: authenticity failure, ... this is a
   signer or configuration fault` and emit nothing. Booking (step 4) has already happened and does
   not depend on this check.

### Authenticity check (`ecrecover`)

After normalization the callback calls `env::ecrecover` on the **same hash the contract computed and
sent to the signer** (`evm::signing_hash(&tx)`, rebuilt from the `Eip1559Tx` the callback carries, never
a caller-supplied value) with the normalized `(r, s, y_parity)` and the malleability flag set (high
`s` is refused, as Ethereum does). It keccaks the recovered public key and compares its last 20 bytes
to `EvmConfig.sender`. `(r, s)` with parity `y` and `(r, n-s)` with parity `y^1` recover the same key,
so checking the normalized triple is the check an Ethereum node would make on the emitted bytes.

- **`sender`** is write-once, like `signer_id` and `derivation_path`. The owner sets it to the
  contract's own derived EVM address (the address the signer derives for this contract account and
  `derivation_path`); the contract cannot compute that derivation itself, so it is configuration.
  A zero address is rejected. `get_evm_config` shows it as a `0x` hex address.
- **Property established:** every transaction the contract emits provably recovers to the
  contract's key for the hash the contract computed. A signature by another key, over another
  hash, with the wrong parity, or off the curve is never emitted.
- **Failure is closed, not silent.** A misconfigured `sender` or a faulty signer cannot cause an
  invalid transaction to be emitted; the cost is that the budget is consumed (G1 is unchanged, the
  receipt succeeded) with nothing emitted and a logged authenticity failure naming a signer or
  configuration fault. The check gates emission only; it never changes accounting.
- **`env::ecrecover` needs near-sdk's `unstable` feature,** enabled in `Cargo.toml` with the exact
  `=5.29.1` pin. Its host cost is about 0.28 Tgas.

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
- **`set_evm_config(signer_id, derivation_path, sender, chain_id, max_fee_per_gas, max_priority_fee_per_gas,
  gas_limit)`** (`gas_limit` defaults to 21000). It rejects a zero chain id or gas limit, an empty
  path, a zero or malformed `sender`, a tip above the fee, and a fee times gas that overflows.
- **Views:** `dry_run_evm(caller, recipient, amount)` goes through the same evaluation as the real
  path; `get_evm_config`, `get_evm_nonce(chain_id)`, `is_evm_recipient_allowed`, `get_evm_recipients`.

### Hash-integrity guarantee

No method signs a caller-supplied hash. The only value ever sent as `payload_v2` is
`evm::signing_hash` of a transaction the contract built in the same call from the fixed
`chain_id`, `max_fee_per_gas`, `max_priority_fee_per_gas`, `gas_limit` (owner), the nonce (contract)
and the validated `to` and `value` (agent). Policy was evaluated on exactly those `to` and `value`
in the same receipt. The callback carries the same `Eip1559Tx` back by value, so the transaction it
assembles is the one that was hashed. The sandbox tests compare the hash the mock signer receives
with viem's keccak of viem's serialization. For a token payment the hashed transaction includes the
`data` field the contract built (`to` the token, zero value, the transfer calldata), so the hash
covers what the transaction does; the sandbox compares it with viem's hash of viem's own ABI-encoded
calldata.

### Owner-controlled gas, contract-owned nonce

- **The agent supplies neither gas nor nonce.** Fees and gas limit are fixed by the owner. A
  caller-chosen fee could burn funds; a caller-chosen nonce could replay or skip.
- **The contract owns the nonce.** One counter per chain id, incremented synchronously at request
  time, so two requests can never share a nonce. It cannot be lowered. `raise_evm_nonce(chain_id,
  nonce)` lets the owner raise it for an address that already has history on that chain (the
  capstone address had nonce 1); raising can open a gap but can never reissue a nonce.
- **`signer_id`, `derivation_path` and `sender` are write-once.** Together they fix the sending
  address, and the nonce counter is only valid for one address per chain. Changing any would leave the
  counter describing a different account (or the authenticity check pointing at a different key). Chain id and fees may change.

### Owner recovery: `owner_evm_withdraw(recipient, value, nonce) -> Promise`

The cross-chain counterpart of `owner_withdraw`, with the same pattern: owner-only, works in any
status including `Frozen`, entirely outside policy (no cap, allowlist or window check) and **no
accounting** (nothing is reserved, released or booked). The owner can send the derived address's ETH
to any recipient in any amount. It has two jobs: recovery, and cancellation.

- **Explicit nonce, deliberately outside the agent path's unique-nonce rule.** To cancel a
  signed-but-unbroadcast or stuck agent transaction, the owner signs a replacement at the *same*
  nonce (typically zero value to the sender itself). The fees come from the configuration, and a
  replacement must outbid what it replaces, so raise them with `set_evm_config` first.
- **The counter moves to `max(counter, nonce + 1)`**, synchronously, before the promise. A fresh or
  future nonce advances it past itself, so the agent can never later take a nonce the owner used; a
  replacement of an issued nonce (`nonce < counter`) leaves it unchanged. `nonce == u64::MAX` is
  refused (no nonce after it), as is a malformed recipient or a missing EVM configuration.
- **No blind signing.** The contract builds the `Eip1559Tx` itself (`EvmConfig::build_tx`, the one
  assembler shared with the agent path) from the owner's recipient and value, the configured chain
  and fees and the explicit nonce, computes the hash itself, and sends it to the signer with the
  same call as the agent path (30 Tgas, 1 yoctoNEAR, 25 Tgas reserved for the callback).
- **`on_owner_sign_complete(tx)`** is a private callback. There is nothing to release or book, so it
  changes no state (`&self`; the surface snapshot therefore labels it `view [private]`, which only
  means it does not write). It applies the same authenticity gate as the agent path through the
  shared `authenticate` function: the signature must assemble and `ecrecover` over the contract's
  own hash must yield the configured `sender`. Then it emits the NEP-297 event
  `evm_owner_withdrawal_signed`, with the same data as `evm_payment_signed`; the different name
  tells an out-of-policy owner transaction from an agent payment. Otherwise it logs
  `owner evm withdrawal not emitted: <why>` (an authenticity failure names a signer or
  configuration fault) or `owner evm withdrawal not signed` for a failed receipt, and emits nothing.
- **Gas.** The callback burnt 2.0 Tgas in the sandbox against 25 reserved (less than the agent
  callback's 2.4, as it does not touch storage), so the reserve is unchanged. The call needs the
  same prepaid gas as a request (about 55 Tgas plus its own work; the tests attach 100): the default
  30 Tgas of a plain call fails with "Exceeded the prepaid gas".
- **Signing confinement.** `owner_evm_withdraw` is one of the functions allowed to dispatch `sign`
  (`tests/signing_confinement.rs`; the others are the agent's two payment methods and
  `owner_evm_token_withdraw`). The request-body builder is a separate function that does not
  dispatch, and both callbacks are not dispatchers.

### Owner recovery of tokens: `owner_evm_token_withdraw(token, recipient, amount, nonce) -> Promise`

How `owner_evm_withdraw` recovers ETH: owner-only, any status, outside policy, no accounting, an
explicit nonce, the transaction built and hashed by the contract and checked by `ecrecover` before it
is emitted. This is the same for an ERC-20 balance, which the agent can otherwise move only within
policy, so the derived address's tokens are no longer beyond the owner's reach.

- **Outside policy on every count:** no token policy, native policy, cap, allowlist, window, budget,
  scope or mandate is consulted, and nothing is reserved, released or booked, on the token or on the
  gas. The owner can move any amount of any token to any recipient, with the contract frozen or the
  mandate revoked.
- **The transaction is built by the agent path's own builder.** `EvmConfig::build_token_tx`: `to` the
  token, zero value, `erc20_transfer_calldata(recipient, amount)`, the configured fees and
  `token_gas_limit`. The hash is computed by the contract; no blind signing.
- **Explicit nonce, same counter rule:** `advance_nonce_past` moves the counter to
  `max(counter, nonce + 1)` (shared with `owner_evm_withdraw`), so a replacement of an issued nonce
  leaves it and a fresh or future nonce moves it past itself. Cancelling a stuck agent token payment
  is a zero-amount transfer to the sender at its nonce, with fees raised first.
- **Same callback.** `on_owner_sign_complete` is reused unchanged, so the authenticity gate is the
  same and the event is the same `evm_owner_withdrawal_signed`; the raw transaction carries the
  transfer (`to` the token, `value` 0, the recipient and amount in the calldata). No new callback, no
  new stored state.
- **The agent cannot reach it.** It is owner-only and not in `AGENT_METHODS`, so the runtime refuses
  the agent's key before any contract code runs (`tests/agent_key.rs` tries every exported method).

### ERC-20 token payments: `request_evm_token_payment(token, recipient, amount)`

The firewall inspects what a transaction does, not only how much ETH it sends. The agent supplies only
`(token, recipient, amount)`. **The contract builds the transfer calldata itself and never accepts raw
calldata**; there is no other selector (no `approve`, no general decoding), and the transaction is
`to` = the token, `value` = 0, `data` = `erc20_transfer_calldata(recipient, amount)`, the nonce from the
shared counter, the configured fees and `EvmConfig.token_gas_limit`
(`EvmConfig::build_token_tx`, the one place a token transfer is assembled). The signing hash covers
the data exactly as it does for a native payment, and the callback's `ecrecover` authenticity gate is
unchanged: the transaction is emitted only if the signature recovers to `sender`.

- **A token is an asset.** `AssetId` gains an appended variant `Evm20 { chain_id, token: [u8; 20] }`
  (the existing `Near`, `Nep141` and `Evm` borsh tags are unchanged). It has its own policy, per-tx
  cap, window, lifetime budget, reserved counter and mandate-scope entry, all in the token's base
  units. In JSON the token is the list of its 20 bytes, because the ABI describes it that way.
- **Dual-asset accounting, the correctness core.** A token transfer moves the token amount and
  burns gas, `token_gas_limit * max_fee_per_gas` wei. The amount is accounted against `Evm20` and the
  gas against the native asset `Evm(chain_id)`, as **two reservations in the one request**:
  1. the **transfer leg** is evaluated like any request: authority, the token's policy, the decoded
     recipient on the EVM allowlist, the cap, window and budget, then the mandate's scope;
  2. the **gas leg** (`policy::evaluate_gas`) is evaluated against the native policy: the same
     authority, then the native asset's cap, window and budget. It has no recipient and **no scope**:
     a scope limits what the agent may *move*, gas is a cost, and the native policy is what bounds
     it. A scope that names only the token therefore still allows the transfer;
  3. a token payment needs a native ETH policy. Without one the request reverts with "A token
     transfer needs a native ETH policy for its gas: none is set";
  4. both legs are evaluated before anything changes. A denial reverts with its reason and its leg,
     `Denied: <Reason> (token transfer)` or `Denied: <Reason> (gas, native ETH)`; the transfer leg is
     reported first;
  5. only if both allow, both amounts are reserved (`reserved[Evm20]` and `reserved[Evm]`) before any
     promise exists, so a second request, native or token, sees both. The gas competes with native
     payments for the same native window and budget.
- **Settlement.** `on_sign_complete(reservations, tx)` takes the list of reservations (a native
  payment passes one pair, a token payment two). It releases each exactly once and books each under
  the one G1 rule: nothing if the signer's receipt failed, everything otherwise, whatever the contract
  could read or authenticate. Emission still depends only on the authenticity gate. A failed receipt
  therefore releases the token amount and the gas and books neither; a signature by another key books
  both and emits nothing.
- **`dry_run_evm_token(agent_key, token, recipient, amount)`** returns
  `{ token: Decision, gas: Decision }`, each leg decided independently, so a caller sees which leg
  would refuse before sending.
- **Configuration.** `set_evm_config` takes an optional `token_gas_limit` (default 100,000, above the
  roughly 50,000 to 65,000 that plain and proxied tokens spend on `transfer`), mutable like the fees,
  must be positive, and `max_fee_per_gas * token_gas_limit` must not overflow.
- **Confinement.** The agent's access key may call `request_evm_token_payment` (it is in
  `AGENT_METHODS`), and the function is the third that may dispatch `sign`
  (`tests/signing_confinement.rs`).
- **Boundaries, disclosed.** (a) *Owner recovery of tokens* was missing when the firewall landed and
  is now `owner_evm_token_withdraw` (see Owner recovery of tokens, above). (b) *The outcome is not observed:*
  as for native payments, the contract is outbound-only. It cannot see whether the token call
  succeeds on the destination chain (a token may revert, charge a fee, or return nothing, as USDT
  does); the amount and the worst-case gas are booked on a signed receipt regardless, which can only
  over-count. (c) *Amounts are `u128`,* narrower than the ERC-20 `uint256`; (d) *only `transfer`:*
  `approve`, `transferFrom` and calls to arbitrary contracts are not possible.

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

- **A failed signing leaves a nonce gap.** The owner can fill or cancel it with `owner_evm_withdraw` at that nonce. The nonce was taken at request time. If the signer fails or
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
- **The signature is verified against the configured sender** (see Authenticity check). What is
  not verified is that `sender` is really the signer's derivation for this contract and path: the
  owner supplies it, and a wrong value fails closed (nothing is ever emitted).
- **Gas.** On testnet the whole request burnt about 14 Tgas, of which `on_sign_complete` used 2.1 of
  its 25 reserved; with the `ecrecover` check the sandbox measures 2.4 Tgas, so the reserve is unchanged. The request needs about 55 Tgas prepaid plus its own work (`sign` 30 + callback 25);
  with less, promise creation fails and the whole request reverts with nothing reserved.

### G2: freeze and revoke do not recall a transaction already signed

A bounded, mitigated boundary, deliberately accepted. `freeze` and `revoke_mandate` stop the agent from
getting anything *new* signed (the next request is denied), but they cannot take back an EVM transaction
the signer has already returned: an EIP-1559 transaction carries no expiry, so a signed one stays valid
until its nonce is mined or replaced, and anyone who holds it can broadcast it.

- **The loss is bounded.** That transaction passed `evaluate` and was reserved and booked when it was
  signed (the G1 rule), so it is capped by exactly what applied at signing: the per-transaction cap, the
  window limit, and the grant's lifetime budget. Revoking does not add to it, and a later request cannot
  exceed what was already counted. A token transfer is bounded the same way, on both of its legs.
- **There is a manual recall.** The owner calls `owner_evm_withdraw` (or `owner_evm_token_withdraw`) with
  the *explicit nonce of the signed transaction*, which signs a replacement at that nonce (typically a
  zero-value transfer to the derived address itself, which the timelock lets through at once). If the
  replacement is mined first, the original can never be: a nonce is used once. Raise the fees first with
  `set_evm_config` (a replacement must outbid what it replaces, which is a loosening change and so waits
  out the timelock if one is set; raise them in advance, or do it before enabling the timelock). The
  window is a race against the broadcaster, so it is a mitigation, not a guarantee.
- **An automatic ledger was considered and rejected.** The contract would record each outstanding signed
  transaction and cancel or release it on a freeze. But the contract is outbound-only: it never sees the
  destination chain, so it cannot know when a transaction was mined, and freeing a ledger slot would need
  a trusted off-chain party to acknowledge it. That would add exactly the trusted authority the design
  avoids, to track a risk the three limits above already bound. So there is no outstanding-transaction
  ledger.

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

## Decision log

How a denied request signals its denial: it panics with `Denied: <Reason>` (plus the leg for a token
payment), which reverts everything. The decision log adds one structured event on every agent request
that reaches the policy decision, so a read-only control panel can stream every attempted action with
Minder's verdict and reason, and an auditor can check each decision against the logged inputs.

It is purely additive: it reads and writes no state, changes no decision, accounting, method
signature or borsh layout, and adds no method. `request_local_transfer`, `request_evm_payment` and
`request_evm_token_payment` compute their decision exactly as before and then log it (`src/decision.rs`).
The owner methods are outside policy and log nothing.

**Event** (standard `minder`, version `1.0.0`, event `decision`), one per request:

```json
{ "method": "request_evm_token_payment",
  "agent_key": "ed25519:..." | null,
  "asset": <AssetId>,
  "recipient": "bob.near" | "0x...",
  "amount": "1000",
  "decision": "allow" | "deny",
  "reason": null | "<DenyReason>",
  "leg": null | "transfer" | "gas",
  "legs": [ { "leg": "transfer", "asset": <AssetId>, "amount": "1000",
              "decision": "allow" | "deny", "reason": null | "<DenyReason>" },
            { "leg": "gas", "asset": <AssetId>, "amount": "...", "decision": ..., "reason": ... } ] }
```

- `asset`, `recipient` and `amount` are the request as the agent gave it: for a native EVM payment
  `amount` is the value in wei, for a token payment the token amount. `agent_key` is the key that
  signed the call, or null if it did not come from the contract's own account.
- `legs` are the legs actually evaluated, with the amount policy counted for each: one `transfer` leg
  for a local or native EVM request (for native EVM the outlay, value plus the worst-case fee), and
  for a token payment a `transfer` leg (the token amount) and a `gas` leg (the worst-case gas against
  the native asset). When the transfer leg already denies, the request stops there and the gas leg is
  absent. A token payment with no native ETH policy logs a single `gas` leg denied `AssetNotAllowed`.
- `decision`, `reason` and `leg` are the outcome: allow, or the first denying leg and its reason.

**Emission points.** On an allow the event is logged at the decision point, before anything is
reserved or dispatched, so it sits in the successful receipt. On a deny it is logged **before the
panic**, and the panic and the revert are exactly as before. A revert discards state changes but not
logs: the logs of a failed receipt stay in its execution outcome, so the event of a denied request is
readable there. The sandbox proves this by reading the logs of the failed receipt itself
(`evm_gate::a_denied_request_persists_its_decision_event_in_the_failed_receipt`).

**Not logged.** A request rejected before any policy is consulted reverts without a decision event:
a malformed address, an EVM payment with no EVM configuration, an EVM asset passed to the NEAR entry
point. Those are input errors, not verdicts. Any call to a request method by another account or key
does reach the decision and is logged (`NotAuthorized`, with `agent_key` null or the other key).

## Evaluation order

The first failing check decides the reason: `Frozen`, `NoMandate`, `MandateRevoked`,
`MandateExpired` (`now >= expiry`), `NotAuthorized` (the call is not from the contract's own account, or its key is not the mandate's),
`AssetNotAllowed`, `RecipientNotAllowed`, `PerTxCapExceeded` (`amount > cap`),
`WindowLimitExceeded` (`effective_spend + amount > limit`, or overflow). These are the global
checks (the budget, `MandateBudgetExceeded`, follows the window). A token payment's gas leg runs the
same checks without the recipient allowlist and without the scope. A request that passes them meets the mandate's scope: `AssetOutOfScope`,
`RecipientOutOfScope`, then `PerTxCapExceeded` again for the mandate's own cap.

## Choices that need your review

- **`dry_run` and `dry_run_evm` take an `agent_key` argument** (spec: `asset, amount, recipient`). A
  view has no signer, so without it `NotAuthorized` could never be produced. The real methods use the
  key that signed the call instead.
- **The agent is a function-call key on the contract account** (see Agent authority), not an account.
  The allowance is a fixed 0.25 NEAR and the method list is fixed in code. Flag: the allowance is a
  judgment call; it must exceed the prepaid gas times the gas price of one request.
- **A live mandate's key cannot be granted again**, and revoking does not bump the epoch (the flag is
  the revocation record).
- **`init` caller:** must be the owner or the contract account itself, so nobody else can claim
  ownership by initializing first. Deploy and init in one batched transaction. The owner must not be
  the contract account.
- **`NoMandate` and `MandateRevoked`** are separate reasons so an audit can tell "never granted"
  from "revoked".
- **Spend history survives `remove_asset_policy`**, so removing and re-adding a policy cannot reset
  the window.
- **`num_buckets` is capped at 64** to bound per-asset storage and per-evaluation work.
- **`revoke_mandate` reverts with no mandate; `remove_asset_policy` reverts with no policy;** other
  owner methods are idempotent.
- **Dependency pins:** `near-sdk =5.29.1` (with its `unstable` feature, for `env::ecrecover`) and `near-workspaces =0.23.0`, with `near-sdk-env 0.1.6`
  and `near-sys 0.2.14` in `Cargo.lock`. Newer releases require rustc 1.95; this environment has
  1.94.1.
- **Stored `EvmConfig` layout changed** (the `sender` field, borsh). No migration is written, so state
  written by an earlier build does not deserialize: a testnet deployment from before this change
  needs a fresh account or wiped state, and `set_evm_config` callers must now pass `sender`.
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
`src/lifecycle_tests.rs`, `src/custody_tests.rs`, `src/gate_tests.rs` and `src/mandate_tests.rs`.
Integration tests are in `tests/invariants.rs` (policy), `tests/transfers.rs` (local transfer path),
`tests/custody.rs` (custody), `tests/evm_gate.rs` (the gate) and `tests/agent_key.rs` (the agent key),
and run against a near-workspaces sandbox. In the sandbox the agent signs as the contract account
with its real function-call key.

| Invariant | Test |
| --- | --- |
| Per-tx cap -> `PerTxCapExceeded` | `amount_over_per_tx_cap_is_denied`; unit `policy::caps_are_inclusive` |
| Cumulative spend over window limit -> `WindowLimitExceeded`; the window never under-enforces the limit and over-enforces by at most one slot width | `cumulative_spend_within_one_window_is_capped`; unit `policy::caps_are_inclusive`, `overflowing_total_is_denied` |
| Spend stops counting after a full window | `spend_stops_counting_once_a_full_window_has_passed` (coarse, advances well past the window) |
| Exact sub-bucket boundary: never forgotten early, at most one slot late | unit `window::spend_is_live_for_k_plus_one_slots`, `spend_is_never_forgotten_early_and_at_most_one_slot_late`, `single_bucket_window_counts_for_two_slots`, `oldest_live_slot_does_not_alias_the_current_slot`, `reused_bucket_is_zeroed_before_adding`, `live_sum_adds_across_buckets_and_ages_them_out_one_at_a_time`, `sum_saturates_instead_of_wrapping` |
| Spend history survives policy removal | `reinstating_a_policy_keeps_spend_history` |
| Recipient not allowlisted -> `RecipientNotAllowed` | `recipient_not_on_allowlist_is_denied` |
| No policy -> `AssetNotAllowed` | `asset_without_a_policy_is_denied` |
| A key other than the mandate's -> `NotAuthorized`; so is the right key from any account but the contract's own | `only_the_mandated_agent_is_authorized`; unit `policy::only_the_mandated_key_is_authorized`, `mandate_tests::another_key_on_the_contract_account_is_not_the_agent`, `the_agent_key_is_not_enough_from_any_other_account`; sandbox `agent_key::another_key_on_the_contract_account_is_not_the_agent` |
| A scoped mandate allows what is in scope and denies a globally allowed but out-of-scope asset or recipient with the right reason, on both surfaces; `dry_run` and `dry_run_evm` agree | unit `policy::the_scope_allows_what_it_names_and_denies_the_rest_of_what_the_policy_allows`, `evm_recipients_are_scoped_apart_from_near_recipients`, `an_empty_list_permits_nothing_of_its_kind`; `mandate_tests::a_scoped_mandate_allows_what_is_in_scope_and_names_what_is_not`; `gate_tests::an_asset_scope_that_excludes_the_evm_asset_denies_evm_payments`, `an_evm_recipient_scope_is_enforced_after_the_global_allowlist`; sandbox `agent_key::a_scoped_mandate_confines_the_real_key_to_less_than_the_global_policy` |
| The effective cap is the smaller of the global and the mandate's: tighter binds, looser does not widen | unit `policy::the_effective_cap_is_the_smaller_of_the_global_and_the_mandates`; `mandate_tests::the_effective_cap_is_the_smaller_of_the_global_cap_and_the_mandates`; `gate_tests::the_mandate_cap_applies_to_the_evm_outlay_and_only_ever_narrows` |
| Intersection: an asset or recipient in the scope but not in the global policy is still denied by the global check; the global checks decide first | unit `policy::the_global_checks_come_first_so_a_scope_never_widens`; `mandate_tests::a_scope_cannot_widen_the_global_policy` |
| An unscoped mandate behaves as before scopes (regression) | unit `policy::an_unscoped_mandate_is_the_global_policy_alone`; `mandate_tests::an_unscoped_mandate_behaves_as_it_did_before_scopes`; `gate_tests::an_unscoped_mandate_leaves_the_evm_surface_as_it_was`; every existing sandbox test, which grants without a scope |
| The budget denies a request whose outlay plus committed and in-flight spend of the grant would exceed it (`MandateBudgetExceeded`); it is inclusive; a failed delivery gives it back | unit `policy::the_budget_counts_epoch_usage_plus_the_request_and_is_inclusive`; `budget_tests::the_budget_denies_when_committed_spend_plus_the_request_would_exceed_it`, `a_failed_transfer_gives_its_budget_back`; sandbox `agent_key::a_budget_bounds_what_one_grant_can_spend_and_a_new_grant_starts_afresh` |
| TOCTOU: two in-flight requests that together pass the budget, the second denied before the first settles, by the reserved count | unit `budget_tests::in_flight_requests_count_against_the_budget_before_they_settle`; `gate_tests::the_evm_budget_counts_outlays_in_flight_and_binds_before_the_window` |
| A new grant resets the budget by the epoch bump alone (the stored counter is not touched); spend settled after a re-grant is charged to the new grant | unit `budget_tests::a_new_grant_resets_the_budget_without_writing_anything`, `spend_settled_after_a_regrant_is_charged_to_the_new_grant`; sandbox `agent_key::a_budget_bounds_what_one_grant_can_spend_and_a_new_grant_starts_afresh` |
| The budget and the window are independent and the tighter decides; where both deny the window is the reason | unit `budget_tests::the_budget_binds_before_the_window_when_it_is_the_smaller`, `the_window_binds_before_the_budget_and_the_budget_outlasts_the_window`, `where_both_would_deny_the_window_is_the_reason`; `policy::the_window_is_the_reason_when_both_would_deny_and_the_budget_decides_otherwise` |
| An unset budget is the window alone; a budget set mid-grant counts earlier spend; lowering it denies until the next grant; removing and re-adding a policy does not reset it; it is per asset | unit `budget_tests::an_unset_budget_is_the_window_alone`, `a_budget_set_mid_grant_counts_what_the_grant_already_spent`, `lowering_the_budget_below_what_was_used_denies_until_the_next_grant`, `removing_and_re_adding_a_policy_does_not_reset_the_budget`, `the_budget_is_per_asset`; `policy::no_budget_means_the_window_alone_however_much_was_spent` |
| A re-grant replaces the scope; scopes are validated and stored normalized; only the owner sets one | unit `mandate_tests::a_regrant_can_change_the_scope_and_the_new_scope_applies_from_the_new_grant`, `a_scope_is_validated_and_stored_normalized`, `only_the_owner_sets_a_scope_and_the_scope_is_visible` |
| Grant provisions a function-call key scoped to the three agent methods, on the contract itself, with a limited allowance | unit `mandate_tests::a_grant_provisions_a_function_call_key_limited_to_the_two_agent_methods`, `the_key_can_reach_only_methods_that_exist_on_the_contract`; sandbox `agent_key::a_grant_puts_a_scoped_function_call_key_on_the_contract_account` (reads the key back from the chain) |
| The real key is refused by the protocol everywhere but the three agent methods | sandbox `agent_key::the_protocol_refuses_the_agent_key_every_method_but_the_two_agent_ones` (every other exported method, no contract logic runs), `the_agent_key_cannot_call_another_contract_such_as_the_signer`, `the_agent_key_cannot_do_anything_but_call_the_two_methods`; and it passes the new auth on both: `the_agent_key_passes_the_new_authentication_on_both_agent_methods` |
| Revoke flags the mandate and deletes the key; the flag denies independent of the delete; a failed delete does not fail the revoke | unit `mandate_tests::revoking_flags_the_mandate_and_deletes_the_key`, `a_request_after_revoking_is_denied_by_the_flag_with_no_deletion_landed`, `revoking_twice_is_a_no_op_and_issues_no_second_deletion`; sandbox `agent_key::revoking_deletes_the_key_and_the_deleted_key_can_no_longer_reach_the_contract`, `after_a_revoke_the_epoch_guard_denies_even_if_the_key_survived_the_delete`, `revoking_when_the_key_is_already_gone_does_not_fail` |
| Re-grant reissues a fresh key and epoch; replacing a live mandate swaps the keys in separate receipts; the live key cannot be granted again | unit `mandate_tests::granting_after_a_revocation_provisions_a_fresh_key_under_a_new_epoch`, `replacing_a_live_mandate_deletes_the_old_key_and_adds_the_new_one`, `the_live_key_cannot_be_granted_again`; sandbox `agent_key::granting_after_a_revoke_reissues_a_fresh_key_and_epoch`, `replacing_a_live_mandate_swaps_the_keys_on_the_account`, `the_key_of_a_live_mandate_cannot_be_granted_again` |
| Expiry denies through `evaluate` and leaves the key | unit `mandate_tests::an_expired_mandate_denies_even_for_its_own_key`; sandbox `agent_key::an_expired_mandate_denies_through_evaluate_while_the_key_stays_on_the_account` |
| `init` rejects the contract account as owner | unit `mandate_tests::init_rejects_the_contract_account_as_owner`; sandbox `agent_key::init_rejects_the_contract_account_as_the_owner` |
| Deployment step: with the deployer key removed the account holds only the agent key and owner control is intact | sandbox `agent_key::removing_the_deployer_key_leaves_only_the_agent_key_and_control_stays_with_the_owner` |
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
| Authenticity: a real MPC signature recovers to the signer's derived address (ground truth from `v1.signer-prod.testnet`: hash `c88d...9536`, address `0xec38...a58c`) and does not for another hash, parity or `s`; high `s` and an off-curve `r` are refused | unit `evm::a_real_mpc_signature_recovers_to_the_derived_address`, `the_real_signature_does_not_recover_to_that_address_for_anything_else`, `recovery_refuses_a_high_s_signature`, `a_point_off_the_curve_recovers_to_nothing`, `independent_fixture_signatures_recover_to_their_keys` (viem signatures, `vectors/evm-signatures.json`) |
| Authenticity failure: a well-formed signature that does not recover to `sender` is booked, emits nothing and is logged as a signer or configuration fault | unit `gate_tests::a_signature_by_another_key_is_booked_but_not_emitted`, `a_signature_over_a_different_transaction_is_booked_but_not_emitted`, `a_signature_that_recovers_to_nothing_is_booked_but_not_emitted`, `a_flipped_recovery_id_is_booked_but_not_emitted`, `a_sender_that_differs_from_the_signing_key_blocks_emission`; sandbox `evm_gate::a_genuine_signature_by_another_key_is_booked_but_not_emitted` |
| The callback has gas to spare for the check | sandbox `evm_gate::the_callback_has_gas_to_spare_for_the_authenticity_check` (about 2.4 of 25 Tgas) |
| Only a failed receipt books nothing | unit `gate_tests::only_a_failed_receipt_books_nothing`, `a_failed_signer_call_releases_and_commits_nothing`; sandbox `evm_gate::a_failing_signer_releases_the_reservation_and_settles_nothing` |
| Callback never panics, even over-releasing | unit `gate_tests::settlement_never_panics_even_if_it_releases_more_than_is_reserved`, `settlement_is_independent_of_changes_made_in_flight`; `gate::tests::malformed_responses_are_rejected_without_panicking` |
| TOCTOU: a second request sees the first's reservation before its callback | unit `gate_tests::a_second_request_sees_the_first_ones_reservation`; sandbox `evm_gate::a_second_request_sees_the_first_ones_reservation_before_it_settles` (observes the reservation mid-flight, the denial, then both nonces settle) |
| Fees count against caps and the window; tiny payments cannot drain through fees | unit `gate_tests::fees_count_toward_the_per_transaction_cap`, `many_tiny_payments_cannot_drain_through_fees`; sandbox `evm_gate::denied_requests_change_nothing_and_never_reach_the_signer` |
| EVM allowlist is separate from the NEAR one; denials take no nonce and reach no signer | unit `gate_tests::the_evm_allowlist_is_separate_from_the_near_allowlist`, `a_denied_request_changes_nothing`; sandbox `evm_gate::denied_requests_change_nothing_and_never_reach_the_signer` |
| No stranded reservation or nonce on a reverted request | sandbox `evm_gate::a_request_without_gas_for_the_callback_reverts_and_reserves_nothing`, `evm_payments_need_configuration_and_a_valid_address` |
| EVM config is owner-only; identity (signer, path and sender) is write-once; nonce only rises | unit `gate_tests::only_the_owner_sets_the_evm_config`, `only_the_owner_sets_evm_recipients`, `only_the_owner_raises_the_nonce`, `fees_and_chain_can_change_but_the_signing_identity_cannot`, `the_config_rejects_unusable_values`, `the_nonce_can_only_be_raised`; sandbox `evm_gate::evm_configuration_is_owner_only`, `the_signing_identity_is_fixed_but_fees_can_change`, `the_nonce_can_be_raised_but_never_lowered` |
| NEAR entry points refuse EVM assets | unit `gate_tests::the_local_entry_points_refuse_evm_assets`; sandbox `evm_gate::the_near_entry_points_refuse_evm_assets` |
| Signing callback is private | sandbox `evm_gate::only_the_contract_itself_can_call_the_signing_callback` |
| `try_assemble_signed` matches viem, normalizes high `s`, rejects every invalid input and never panics | unit `evm::assembling_a_valid_signature_matches_viem`, `assembling_a_high_s_signature_normalizes_it_to_the_viem_encoding`, `assembling_rejects_each_invalid_input`, `assembling_never_panics_on_any_combination_of_edge_inputs` |
| Signer response parsing and the event format | unit `gate::tests::a_well_formed_response_is_assembled`, `invalid_signatures_are_rejected_with_the_reason`, `the_event_carries_the_transaction_fields_and_raw_bytes` |
| End to end on testnet | the capstone above: real `v1.signer-prod.testnet`, real Sepolia confirmation (not an automated test) |
| Hex and address helpers | unit `evm::hex_round_trips_and_rejects_bad_input`, `addresses_parse_and_format` |
| `owner_evm_withdraw` is owner-only (the agent's key is refused by the protocol, a stranger by the owner check) | unit `gate_tests::only_the_owner_can_withdraw_cross_chain`; sandbox `evm_gate::only_the_owner_can_withdraw_cross_chain`, `agent_key::the_protocol_refuses_the_agent_key_every_method_but_the_two_agent_ones` |
| It works while `Frozen`, with the mandate revoked or no policy, and emits viem's signed transaction | unit `gate_tests::it_works_while_frozen_and_emits_the_signed_transaction`, `it_works_with_the_mandate_revoked_and_no_policy`; sandbox `evm_gate::the_owner_can_withdraw_while_frozen_outside_policy_with_no_accounting` |
| It is outside policy and touches neither `reserved` nor the window (over the cap, non-allowlisted recipient, in flight and committed spend unchanged through dispatch and every callback outcome) | unit `gate_tests::it_is_outside_policy_and_touches_neither_reserved_nor_the_window`; sandbox `evm_gate::the_owner_can_withdraw_while_frozen_outside_policy_with_no_accounting`, `a_failed_signer_call_changes_nothing_for_the_owner` |
| The explicit nonce is used; the counter becomes `max(counter, nonce + 1)`; a replacement leaves it; the agent never takes a used nonce | unit `gate_tests::the_explicit_nonce_is_used_and_the_counter_moves_to_max_of_counter_and_nonce_plus_one`, `a_cancellation_is_a_zero_value_transfer_to_the_sender_itself`, `it_rejects_a_malformed_recipient_and_an_exhausted_nonce`; sandbox `evm_gate::the_counter_follows_max_of_counter_and_nonce_plus_one_and_the_agent_never_collides` |
| Hash integrity: the signer is sent the hash of the transaction the contract built, and the callback carries it | unit `gate_tests::the_signer_is_sent_the_hash_of_the_transaction_the_contract_built`; sandbox `evm_gate::the_owner_can_withdraw_while_frozen_outside_policy_with_no_accounting` (viem's hash) |
| Owner path authenticity: a signature by another key, or a mismatched `sender`, emits nothing and is logged; unusable results emit nothing and book nothing | unit `gate_tests::a_signature_by_another_key_is_not_emitted_for_the_owner_either`, `a_mismatched_sender_configuration_emits_nothing_for_the_owner`, `unusable_signer_results_emit_nothing_and_book_nothing_for_the_owner`; sandbox `evm_gate::a_signature_by_another_key_is_not_emitted_for_the_owner`, `a_mismatched_sender_configuration_emits_nothing_for_the_owner` |
| The owner callback has gas to spare | sandbox `evm_gate::the_owner_callback_has_gas_to_spare` (about 2.0 of 25 Tgas) |
| Signing is confined to `request_evm_payment` and `owner_evm_withdraw`; the public surface is the committed snapshot | `tests/signing_confinement.rs`; `tests/method_surface.rs` against `tests/snapshots/method-surface.txt` |
| The firewall builds exactly the transfer calldata (selector, padded recipient, amount) and the signing hash covers the data | unit `gate_tests::the_firewall_builds_exactly_the_transfer_calldata_and_hashes_it`, `the_calldata_matches_viems_abi_encoding`, `evm::erc20_transfer_vector` (viem); corpus `evm_corpus::encoder_matches_viem_byte_for_byte_over_the_whole_corpus` (340 token cases, calldata rebuilt and compared); sandbox `evm_gate::the_agent_key_pays_a_token_the_contract_builds_and_both_assets_are_accounted` (viem's hash and signed bytes) |
| Policy on both legs: over the token cap, gas over the native policy, no native policy, no token policy, off-allowlist decoded recipient, each reverting with its leg | unit `gate_tests::a_transfer_over_the_token_cap_is_denied_on_the_token_leg`, `gas_that_would_exceed_the_native_policy_is_denied_on_the_gas_leg`, `a_token_transfer_with_no_native_policy_is_denied_clearly`, `a_token_without_a_policy_is_denied`, `the_decoded_recipient_must_be_on_the_evm_allowlist`; sandbox `evm_gate::token_denials_name_the_leg_and_change_nothing` |
| The mandate scope applies to the token and the decoded recipient and its cap, not to the gas | unit `gate_tests::the_mandate_scope_applies_to_the_token_and_the_recipient_but_not_to_the_gas`; `policy` gas-leg checks via the harness |
| Dual reservation is TOCTOU-safe; both legs settle or release exactly once; a failed receipt books neither; authenticity still gates emission (G1 on both legs) | unit `gate_tests::both_reservations_are_made_together_and_the_second_request_sees_them`, `a_successful_receipt_books_both_assets_and_a_failed_one_releases_both_and_books_none`, `a_token_transfer_signed_by_another_key_is_booked_on_both_assets_but_not_emitted`; sandbox `evm_gate::an_in_flight_token_payment_holds_both_reservations_and_releases_both_when_it_fails`, `a_failing_signer_releases_both_legs_and_books_neither` |
| Native ETH path unchanged; token and native payments share the nonce counter; the token gas limit is configurable | unit `gate_tests::a_native_payment_is_unchanged_by_token_support`, `token_payments_share_the_nonce_counter_with_native_payments`, `the_token_gas_limit_is_configurable_and_must_be_positive`, `the_native_entry_points_refuse_token_assets`; every native test above |
| `owner_evm_token_withdraw` is owner-only; the agent's key is refused by the protocol | unit `gate_tests::only_the_owner_can_recover_tokens`; sandbox `evm_gate::only_the_owner_can_recover_tokens_and_the_agent_key_is_refused_by_the_protocol`, `agent_key::the_protocol_refuses_the_agent_key_every_method_but_the_three_agent_ones` |
| It works while `Frozen` and is outside policy for any token, amount and recipient (no token or native policy needed), emitting viem's signed transfer | unit `gate_tests::it_works_while_frozen_and_emits_viems_serialization_of_the_transfer`, `it_is_outside_policy_for_any_token_amount_and_recipient`; sandbox `evm_gate::the_owner_recovers_tokens_while_frozen_outside_policy_with_no_accounting` |
| No accounting: reserved, window and budget of the token and of ETH are unchanged through dispatch and every callback outcome | unit `gate_tests::it_touches_neither_reserved_nor_the_window_nor_the_budget_of_either_asset`; sandbox as above |
| The signer is sent the hash of the transfer the contract built (calldata checked against viem); the token gas limit is the configured one | unit `gate_tests::the_signer_is_sent_the_hash_of_the_token_transfer_the_contract_built`, `the_owner_token_transfer_uses_the_configured_token_gas_limit`; sandbox (viem's hash `HASH_OWNER_TOKEN_OVER_CAP`) |
| Explicit nonce used; counter `max(counter, nonce + 1)`; a replacement leaves it; the agent never takes a used nonce | unit `gate_tests::the_explicit_nonce_is_used_and_the_counter_moves_to_max_of_counter_and_nonce_plus_one_for_tokens`, `it_needs_the_evm_configuration_and_valid_addresses_and_a_usable_nonce`; sandbox `evm_gate::a_token_cancellation_leaves_the_counter_and_a_future_nonce_moves_it_past_itself` |
| Authenticity: a signature by another key, or a mismatched `sender`, emits nothing | unit `gate_tests::a_signature_by_another_key_is_not_emitted_for_a_token_recovery`, `a_mismatched_sender_configuration_emits_nothing_for_a_token_recovery`; sandbox `evm_gate::a_token_recovery_signed_by_another_key_or_a_mismatched_sender_is_not_emitted` |
| The token callback has gas to spare | sandbox `evm_gate::the_token_callback_has_gas_to_spare` (about 3.2 of 25 Tgas) |
| One decision event per agent request, allow and deny, with the inputs and the verdict, on all three methods (a token request's two legs included); a request rejected before policy logs none | unit `decision::tests::an_allow_carries_the_inputs_and_every_leg`, `a_deny_names_the_first_denying_leg_and_its_reason`; `lifecycle_tests::an_allowed_local_request_logs_an_allow_with_its_inputs`, `a_denied_local_request_logs_the_deny_before_it_reverts`, `every_deny_reason_of_the_local_path_is_logged_as_the_reason_it_reverts_with`, `a_call_from_another_account_is_logged_with_no_agent_key`, `a_request_rejected_before_policy_logs_no_decision`; `gate_tests::an_allowed_native_payment_logs_an_allow_with_the_value_and_the_outlay`, `a_denied_native_payment_logs_the_deny_before_it_reverts`, `an_allowed_token_payment_logs_both_legs`, `a_token_payment_denied_on_the_transfer_leg_logs_only_that_leg`, `a_token_payment_denied_on_the_gas_leg_logs_both_legs`, `a_token_payment_with_no_native_policy_logs_the_gas_leg_denial` |
| A deny's decision event persists in the FAILED receipt's logs; allows are in the successful receipt; execution events and owner methods are unchanged | sandbox `evm_gate::a_denied_request_persists_its_decision_event_in_the_failed_receipt`, `an_allowed_request_logs_an_allow_in_the_successful_receipt`; unit `gate_tests::the_execution_events_and_the_owner_methods_are_unchanged_by_the_decision_log` |
| Instant: freeze, revoke, a tightening or equal policy change, removing an asset or recipient, a grant that does not extend a live mandate, recovery to the treasury and to the derived address, raising the delay | unit `timelock_tests::freeze_and_revoke_take_effect_at_once`, `a_policy_change_that_is_tighter_or_equal_in_every_field_is_instant`, `removing_an_asset_or_a_recipient_is_instant`, `a_grant_that_does_not_extend_a_live_mandate_is_instant`, `recovery_to_the_treasury_and_to_the_derived_address_is_instant`, `raising_the_delay_is_instant_and_the_timelock_can_be_enabled_later`; sandbox `timelock::a_compromised_owner_cannot_loosen_or_redirect_at_once_but_can_still_freeze` |
| Loosening is delayed: a raised cap, window or budget, a new asset, a new recipient (NEAR and EVM), an extended live mandate, unfreeze, a higher EVM fee or gas limit, a lowered delay, a changed treasury, and recovery to any other destination each take effect only through propose, the delay and execute; early execution is refused; cancel removes the change | unit `timelock_tests::a_raised_cap_window_or_budget_is_delayed`, `adding_an_asset_is_delayed`, `adding_a_recipient_is_delayed_on_both_surfaces`, `extending_a_live_mandate_is_delayed`, `unfreezing_is_delayed_but_freezing_is_not`, `loosening_the_evm_configuration_is_delayed_and_tightening_it_is_not`, `lowering_the_delay_or_changing_the_treasury_is_delayed`, `changing_the_treasury_is_delayed`, `recovery_to_any_other_destination_is_delayed`, `cancel_removes_a_pending_change_and_it_can_no_longer_be_executed`; sandbox `timelock::a_loosening_change_takes_effect_only_after_propose_wait_and_execute`, `a_cancelled_change_never_takes_effect`, `owner_recovery_is_instant_to_the_treasury_and_delayed_anywhere_else`, `evm_gate::evm_recovery_is_instant_to_the_treasury_and_to_the_derived_address_and_delayed_elsewhere` |
| A mixed tightening and loosening policy change is loosening | unit `timelock_tests::a_mixed_tightening_and_loosening_policy_change_is_loosening` |
| The compromised-owner scenario: raising a cap, adding the attacker as a recipient, redirecting the treasury, dropping the delay and withdrawing to the attacker's address are each forced through the delay while a legitimate freeze and a cap-lowering stay instant; tightening works while a loosening is pending | unit `timelock_tests::a_compromised_owner_key_cannot_widen_authority_or_redirect_funds_at_once`, `tightening_works_while_a_loosening_of_the_same_thing_is_pending`; sandbox `timelock::a_compromised_owner_cannot_loosen_or_redirect_at_once_but_can_still_freeze` |
| Mechanism: unique never-reused ids, id-ordered listing, only loosening and valid changes can be proposed, owner-only, a bounded queue, logged events, timelock off means everything instant | unit `timelock_tests::ids_are_unique_and_pending_changes_are_listed_in_id_order`, `only_a_loosening_change_can_be_proposed_and_only_a_valid_one`, `only_the_owner_can_propose_execute_or_cancel`, `the_number_of_pending_changes_is_bounded`, `proposals_executions_and_cancellations_are_logged`, `with_the_timelock_off_every_change_is_instant_and_nothing_can_be_proposed`, `a_grant_whose_time_has_passed_by_execution_reverts_and_stays_pending`; sandbox `timelock::the_timelock_can_be_enabled_after_arming_and_never_loosened_instantly`, `init_takes_the_delay_and_the_treasuries` |
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
- The agent is identified by public key, as in production: its calls come from the contract account
  signed with the mandate's key, and every grant rotates between two keys (the contract refuses to
  re-grant the live one). A `StaleKeyRequest` action signs a request with the other, stale key.
- Most suites boot with the timelock off, so their owner actions are as they always were. The
  `timelock` suite and the random driver boot **timelocked** (armed first, then the delay is raised, as
  a deployment is). There the model states, from the classification rule alone, which owner calls must
  apply and which must be refused as timelocked, tracks the pending changes with their ids and
  ready-at times, and the harness proposes, executes (early and due) and cancels them.
- A third asset, a token, makes a request a payment with two legs: the harness reserves the token amount
  and the gas against the native asset, expects the contract's callback to carry both, rebuilds the
  transfer calldata byte by byte on its own, and checks that the view (`dry_run_evm_token`) and the
  real path agree with the oracle on each leg (the transfer leg decided first, with its recipient and
  scope, the gas leg by the native policy alone). A delivery releases both and books both or neither.
- Policies include two with a lifetime budget (70 and 150, over the default cap and window). The
  oracle keeps per-asset, per-grant committed spend that never ages out and a grant resets, and counts
  in-flight spend against it. Because that counter is part of the state, it separates states the
  window alone would fold together, so the older suites explore more states than before.
- Every grant also carries one of six scopes (none, NEAR only, exactly the allowed recipients, only
  the disallowed recipients, a cap of 40, a cap of 100). The shadow model states their effect
  independently, after the global checks, so the contract's decision must equal the intersection.
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
| accounting (`exhaustive_accounting_across_interleavings`) | 9 | 22,328 | 90,887 | 65,178 | 2,812 | requests at and below the cap that press the limit; deliveries of `Ok`, `Fail`, `WrongShape`, `Oversized` in either order; a window-length advance |
| authority (`exhaustive_authority_and_time`) | 11 | 45,534 | 185,244 | 127,867 | 39,860 | freeze, unfreeze, revoke, short and long grants (each rotating the agent key), slot advances, requests with the current and the stale key, interleaved with in-flight requests |
| policy (`exhaustive_policy_changes_and_caps`) | 11 | 13,486 | 73,721 | 54,642 | 11,749 | tightening and loosening limits under in-flight spend, over-cap and non-allowlisted requests, `Garbage` delivery |
| scope (`exhaustive_mandate_scopes`) | 10 | 24,854 | 104,843 | 79,990 | 6,517 | grants of each kind of scope (NEAR only, only the disallowed recipients, a tight cap, none) interleaved with requests on both surfaces to allowed and disallowed recipients, a loosened limit and in-flight spend |
| budget (`exhaustive_epoch_budgets`) | 10 | 60,702 | 237,500 | 163,103 | 30,220 | lifetime budgets of 70 (binds before the window of 100) and 150 (after it) on both assets, set and replaced under in-flight spend, failed and successful deliveries, window ageing, and a re-grant that resets the budget, including a delivery that lands after it |
| token (`exhaustive_token_payments`) | 10 | 21,652 | 91,702 | 70,051 | 9,358 | token payments with their two legs interleaved with native payments that share the gas asset, a tight native policy, a token budget, scopes that exclude the token, and successful and failed deliveries |
| timelock (`exhaustive_timelock`) | 12 | 38,233 | 128,403 | 89,835 | 18,381 | policy changes that tighten (instant) and loosen (refused when called, held when proposed), unfreeze and grants under the timelock, execution only when due, cancellation, requests, freezes and the clock, in a world booted timelocked |
| **Total** | | **226,789** | **912,300** | | | |

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
| The decision log: on every request, allowed or panicked, exactly one decision event is emitted and it equals the one restated from the model's decisions and the request's inputs (method, key, asset, recipient, amount, each evaluated leg with its amount and verdict, and the first denying leg) | yes | yes | yes | yes | |
| The timelock: every owner policy change, unfreeze and grant is classified as the model's independent restatement says (a loosening one refused when called and held when proposed, a tightening one applied at once); the pending list, ids and ready-at times equal the model's; a change is executed only when due and cancelled at will; no loosening takes effect before its delay; and every asset's stored policy equals the model's (reached by the random driver, which runs timelocked, and the `timelock` suite) | | | | yes | |
| Token payments: both legs are decided as the oracle says (the transfer by the token's policy, allowlist and scope; the gas by the native policy alone), both are reserved together and counted by the next request, the callback carries both reservations and the exact transfer transaction with its calldata, and a delivery releases and books both or neither (reached by the random driver and the `token` suite) | | | | yes | |
| The lifetime budget: the contract's per-epoch committed spend (read through its lazy-reset accessor) equals the oracle's after every step; budget usage, committed plus in flight, never passes the budget unless the owner lowered it; a new grant resets it, a delivery after the re-grant is booked to the new grant, and the decision (allow or `MandateBudgetExceeded`, with the window deciding first when both deny) equals the oracle's (reached by the random driver and the `budget` suite) | | | | yes | |
| The agent's authority is the intersection of the policy and the mandate's scope: the shadow model restricts every request to what both allow, with the scope checked after the global ones (out-of-scope asset or recipient, the tighter cap), so an allowlist or cap is never widened by a scope, and the stored scope equals the granted one (reached by the random driver and the `scope` suite) | | | | yes | |
| Agent authentication is by key: every grant switches to the other of two keys, and a request signed with the key the mandate no longer names is never approved (`NotAuthorized` once the mandate checks that rank above it pass), and `dry_run` agrees | | yes | | yes | |
| EVM nonce (agent path; `owner_evm_withdraw` is deliberately outside this rule and is not in the harness's alphabet): the counter equals the oracle, strictly increases, no `(chain, nonce)` is reused, and a failed signing still consumes its nonce | yes | yes | yes | yes | |
| Dispatch integrity: an allowed request builds exactly the transaction policy approved (owner-set fees, contract nonce, validated `to` and `value`), the callback carries it, and the signer is sent that transaction's hash and the configured path; local callbacks carry the right asset and amount | yes | yes | yes | yes | |
| G1 booking rule: only a failed receipt books nothing; a successful-but-unusable receipt books the outlay | `WrongShape`, `Oversized`, `Fail` | `Ok`, `Fail` only | `OtherSigner` | all | |
| The signed-transaction event is emitted if and only if the signature assembles and recovers to the configured sender (`OtherSigner` is genuine but by another key), and an unusable or inauthentic result is logged as booked with nothing emitted | yes | yes | yes | yes | |
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

## Formal verification (Kani)

Bounded model checking of the pure modules with [Kani](https://github.com/model-checking/kani) 0.68.0,
alongside the differential corpus (the encoder against viem, byte for byte) and the stateful harness
(the contract against a shadow model). Kani proves a property for *every* input within stated bounds; the
corpus and the harness sample.

**Where.** `contract/kani/` is a crate of its own, outside the contract's build. It includes
`src/window.rs` and `src/evm.rs` by path (the shipped files, not copies) and holds the proofs in
`contract/kani/src/proofs.rs`, compiled only under `cargo kani`. The contract's manifest, sources,
tests and wasm do not see it. Its near-sdk dependency is pinned to the same `=5.29.1` with
`non-contract-usage`, which is what lets near-sdk compile for the host; no host function is reachable
from the code under proof. The secp256k1 order is restated in the proofs from the standard, so a wrong
constant in the module cannot hide in its own proof.

**Run.**

```
cargo install --locked kani-verifier && cargo kani setup     # once; Kani installs its own nightly
cd contract/kani && cargo kani                               # all 11 proofs, about 10 minutes
cd contract/kani && cargo kani --harness normalize_s_is_total_and_always_low_s   # one proof
```

**Proved** (each verified with 0 failed checks on Kani 0.68.0):

| Harness | Property | Bounds |
| --- | --- | --- |
| `normalize_s_is_total_and_always_low_s` | For every `s` in `1..n` and recovery id 0 or 1, `normalize_s` does not panic; the output is `1 <= s' <= n/2` with `y_parity` 0 or 1; a low `s` passes through, otherwise `s' = n - s` (checked as a 256-bit sum) and the parity flips | none: all 2^256 values of `s` |
| `normalized_signature_is_total_and_yields_only_valid_signatures` | For every affine point of 0 to 34 bytes, scalar of 0 to 34 bytes and recovery id, `normalized_signature` does not panic, and an accepted signature has `1 <= r < n` (`r` is the point's x), `1 <= s <= n/2`, `y_parity` 0 or 1, from a 33-byte point with prefix 02 or 03 and a 32-byte scalar | lengths stop at 34: every longer input takes the same early return as 34; the bytes are fully symbolic |
| `encoding_any_recipient_never_panics` | Encoding (unsigned and signed) a transaction with any recipient address does not panic | the rest of the transaction is a fixed base transfer |
| `encoding_any_55_bytes_of_data_never_panics`, `encoding_any_56_bytes_of_data_never_panics` | The same for any calldata of 55 bytes (the longest short RLP string) and of 56 (the shortest long one) | the lengths; every byte symbolic |
| `the_ring_never_under_counts_with_{1,2,3}_slots` | For arbitrary spends and a query at an arbitrary later time, the sliding window counts at least the true spend inside the window (the fail-safe direction the policy relies on), over windows of 1, 2 and 3 slots | see below |
| `the_ring_holds_spend_at_most_one_slot_too_long_with_{1,2,3}_slots` | And at most the spend made within one slot beyond the window (it over-enforces by no more than a slot width) | see below |

The window proofs make 3 spends at arbitrary non-decreasing times and amounts (`u8`, so the true sum
cannot overflow) and a later query; `K` is fixed per proof, the slot length is 1 to 8 ns and times are
below 1,600 (about 200 slots, far past the ring's span, so every index wrap and every expiry is
reached). Wider bounds (slot lengths to 2^20, times to 2^32) did not finish in the time allowed, and a
symbolic `K` or full 64-bit times exhausted memory. Saturation of a sum is a separate argument and a
fail-safe in itself (a saturated total exceeds every limit).

Together the first two proofs close the argument that matters at the signing boundary: whatever the
signer returns, `normalized_signature` either refuses it or produces exactly the `(r, s, y_parity)` that
`encode_signed` and the authenticity check assume.

**Not proved, and why.** Panic-freedom of `try_assemble_signed` *end to end*, and of `encode_signed`
over every `r`, `s` and numeric field, is **not** proved. The RLP integer encoder trims a symbolic number
of leading zeros and copies a symbolic-length slice into a `Vec`, and CBMC exhausted memory (11 GB) on
every harness that made an integer or a signature symbolic (chain id, nonce, gas, fees, value, `r`, `s`,
a single data byte), at several unwinding depths. Those harnesses were removed rather than left failing.
For that remaining part the floor is the differential corpus (1,743 cases, byte for byte against viem,
including the integer widths and RLP edges) and the byte-level fuzz tests (10,000 cases per property,
`try_assemble_signed_never_panics_and_accepts_exactly_the_acceptable_signatures` among them). The
sliding window is proved only inside the bounds above; the stateful harness covers the rest.

**Environment note.** `cargo kani setup` installs Kani's own nightly toolchain next to the pinned
stable 1.94.1; the pinned toolchain is not changed.

## Running

```
cargo near build non-reproducible-wasm --locked   # production wasm, no test helper
contract/test.sh                                  # the whole test suite: the canonical command
```

**The canonical test command** is `contract/test.sh` (equivalently `cargo test-all` in `contract/`, an
alias for `cargo test --no-fail-fast`). It runs every test binary even if one fails, so one failure never
hides the rest; prints one line per binary and its result; and exits non-zero on any failure. Extra
arguments go to `cargo test` (`contract/test.sh --test evm_gate`). It needs Rust 1.93 or newer and
`cargo-near` (`cargo install --locked cargo-near`), which the sandbox tests use to build the contract and
the test doubles; the first run downloads the near-sandbox binary. A full run takes about ten minutes.

**Why the settings in `.cargo/config.toml` matter.** The sandbox integration tests start one near-sandbox
node per test, several at once. Under CPU load a node could take longer than the default 10 seconds to
answer its first RPC call, and the test then failed to start with `SandboxStartupRetriesExhausted`, with
nothing wrong in the contract. Both near-sandbox and near-workspaces read `NEAR_RPC_TIMEOUT_SECS` for that
readiness window, so the repository's `.cargo/config.toml` sets it to 90 seconds for every cargo
invocation under the repository (an environment variable you set yourself still wins). Waiting longer
costs nothing when a node is quick, since the wait ends at its first answer. It was verified by three
consecutive full runs of `contract/test.sh`, each exit 0 with no start-up failure and the default thread
count (so no cap on test threads was needed): 243 unit, 17 `agent_key`, 11 `custody`, 2 `evm_corpus`, 38
`evm_gate`, 15 `invariants`, 4 `method_surface`, 9 `signing_confinement`, 6 `timelock` and 8 `transfers`
tests passed in every run. Before the setting, runs under load intermittently failed whole binaries
(`custody`, `invariants`) at node start-up. The Kani proofs (`contract/kani`, see Formal verification) are separate and are not
part of the suite.
