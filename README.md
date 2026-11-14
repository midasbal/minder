# Minder

**A policy firewall between an AI agent and money, as a NEAR smart contract.** Assume the agent is already
compromised. Contain it, and prove the containment.

The agent never holds funds. It holds one narrow, revocable, time-boxed key that can do almost nothing on its
own: it can ask the contract to pay, and the contract answers every request with the same deterministic policy.
Testnet only; see [Status](#status).

## The problem

Agents are getting wallets. The same properties that make them useful (they read untrusted text, call tools,
and act without a human in the loop) make them manipulable: a prompt injection, a poisoned tool or a jailbreak
can turn an agent against the person who gave it money. A policy in the agent's own prompt or code does not
help, because the compromised thing is the thing enforcing it.

Minder moves the enforcement out of the agent. The agent can be fully subverted and the damage is still bounded
by limits it cannot change, enforced by code it does not control.

## How it works

- **The agent holds a function-call key and nothing else.** `grant_mandate` adds the agent's public key to the
  contract's own account as a NEAR function-call access key: receiver the contract itself, methods exactly
  `request_local_transfer`, `request_evm_payment` and `request_evm_token_payment`, a fixed 0.25 NEAR gas
  allowance, no deposit. The NEAR runtime enforces that before any contract code runs: the key cannot call
  any other method, transfer, deploy, or add or delete a key. It is **revocable** (`revoke_mandate` deletes it)
  and **time-boxed** (the mandate has an expiry).
- **The contract is the only path to funds.** Every request meets one pure function, `policy::evaluate`: the
  contract is active, the mandate is unrevoked and unexpired, the call carries the mandate's key, the asset has
  a policy and the recipient is allowlisted, the amount is within the **per-transaction cap**, the **sliding
  window** limit and the **epoch budget** (a lifetime budget for one grant of the mandate), and the mandate's
  own **scope** (a subset of assets and recipients, and its own cap) allows it. A scope can only subtract. The
  first failing check decides the reason code the agent gets back.
- **Cross-chain payments, with the contract as the only signer.** For ETH and ERC-20 tokens on an EVM chain the
  contract builds the transaction itself (calldata included), has NEAR Chain Signatures sign it, checks that the
  signature really recovers to the contract's own EVM address, and only then emits the signed transaction as an
  event for anyone to broadcast. The agent never supplies a transaction, a hash or a nonce. Chain Signatures
  derives the signing key from the calling account, so the contract's EVM address is one only this contract can
  sign for.
- **An asymmetric timelock.** Tightening and emergency actions are instant (freeze, revoke, lowering a limit).
  Anything that loosens authority (raising a cap, adding a recipient, extending a mandate, raising fees) waits a
  configurable delay, and a separate **guardian** account can only tighten (`freeze`, `revoke_mandate`,
  `cancel_change`). The delay is the in-contract window in which someone other than the owner can react to a
  compromised owner key; defending the owner key itself is still best done with a multisig owner (`DESIGN.md`).
- **Every request is logged.** Allowed or denied, each request emits a `decision` event (NEP-297) with the
  reason.

What this does not do, stated up front: `freeze` and `revoke_mandate` stop new payments but cannot recall an
EVM transaction the contract has already signed (a signed EIP-1559 transaction has no expiry). The loss is
bounded by the limits that applied at signing, and the owner has a manual replacement path. See `DESIGN.md`,
"G2". The owner's own recovery methods (`owner_withdraw` and its EVM forms) are deliberately outside the policy:
Minder contains the agent, not the owner.

## Two properties you can check

### Keyless custody

The contract account is created by one transaction whose actions are `CreateAccount`, `Transfer`,
`DeployContract` and `FunctionCall(init)`, with **no `AddKey`**. The account therefore has **zero full-access
keys**: nobody, the deployer included, can sign as it, redeploy it or change its code. The only keys it ever
holds are the agent's restricted function-call key, which it adds to itself. Anyone can read the key list from
the chain (`view_access_key_list`), and the verification script and the live console both do.

The cost, from `DESIGN.md`: there is no upgrade path (a bug is fixed by moving to a new account with the owner's
recovery methods), and whoever signs the deployment is the owner permanently.

### Provable, within stated bounds

The guarantees are machine-checked, with the bounds written down next to each result in `contract/DESIGN.md`:

- **43 Kani proof harnesses** (Kani 0.68.0, bounded model checking) over the pure logic: the firewall decision
  (`policy::evaluate`), the timelock classifier (`loosening::loosens`: soundness over every input, completeness
  per variant), the sliding window and the EVM signature normalization. A proof holds for every input *within its
  stated bounds*; the bounds, and what is deliberately not proved (for example, end-to-end panic-freedom of
  `encode_signed`, which exhausted the model checker's memory), are listed in "Formal verification (Kani)".
- **Mutation testing** with `cargo-mutants` 27.1.0: of 259 mutants measured, 247 were viable and **241 were
  killed**; the other 6 are documented equivalents (they cannot change behaviour), none unexplained. The
  classifier has since moved to `src/loosening.rs`, where the Kani proofs supersede the score; that new scope
  (294 mutants) has not been re-run.
- **A 1,743-case differential corpus**: the EVM encoder checked byte for byte against viem
  (`contract/vectors/`), alongside a stateful model-based harness that compares the contract against a shadow
  model.

This is not a claim of complete formal verification. Read the bounds before relying on a result.

## Protect your own agent with Minder

Minder is a reusable primitive, not a one-off demo: any agent that can sign a NEAR function call can be put
behind it, and the agent never has to hold funds.

1. **Deploy a keyless gate.** `contract/deploy/deploy-keyless.sh` creates the account, funds it, deploys the
   wasm and runs `init` in a single batch with no `AddKey`. It prints the full command and sends nothing unless
   you pass `--send`, and refuses any network but testnet.

   ```
   contract/deploy/deploy-keyless.sh deploy --signer owner.testnet --account gate.owner.testnet \
       --fund "7 NEAR" --window-len 24h --num-buckets 24 --timelock-delay 24h --guardian guardian.testnet
   ```

   It needs near-cli-rs, `jq` and `cargo-near`. The signer becomes the owner for good, and the new account is a
   sub-account of the signer.

2. **Set the policy** (owner): `set_asset_policy(asset, per_tx_cap, window_limit, epoch_budget)` per asset,
   `set_recipient` for NEAR recipients, `set_evm_recipient` and `set_evm_config` for EVM payments, and fund the
   account with `deposit`. A timelock delay of zero means every change is instant, which is how a deployment is
   armed; then `set_timelock_delay` switches the delay on (`DESIGN.md`, "Owner timelock").

3. **Grant the agent a mandate.** `grant_mandate(agent_key, expiry, scope, reset_budget)` with the agent's
   ed25519 public key. The contract adds it as the restricted function-call key described above. The private key
   stays with the agent. One mandate is active per contract; deploy one gate per agent.

4. **The agent calls the contract instead of holding money.** Plain NEAR function calls, signed with its key:

   | Method | Arguments |
   | --- | --- |
   | `request_local_transfer` | `asset` (`"Near"` or `{"Nep141": "token.testnet"}`), `amount` (a string, in base units), `recipient` |
   | `request_evm_payment` | `recipient` (a `0x` address), `amount` (wei, as a string) |
   | `request_evm_token_payment` | `token`, `recipient`, `amount` (the token's base units, as a string) |

   Free views let it (or you) preflight a request without spending anything: `dry_run`, `dry_run_evm` and
   `dry_run_evm_token` take the agent's public key and return the contract's own verdict and reason code.
   `get_headroom` and `get_budget_usage` show what is left of each limit.

5. **Operate it.** The guardian can `freeze`, `revoke_mandate` and `cancel_change` at once; anything that
   loosens waits the delay.

There is no packaged client library yet: the calls above are ordinary NEAR function calls, and the console in
`web/` shows them being made against a live deployment. The contract is NEAR-native; the EVM payments are
outbound (the contract signs, someone broadcasts).

## Verify it yourself

```
scripts/verify-deployment.sh
```

Read-only (view calls only, no key, no transaction). It builds the contract wasm from a clean copy of the
committed tree, hashes it, fetches the code the chain holds for `gate.minder-gate-owner-bf5fcd.testnet` and
compares, then reads `view_access_key_list` and checks there are no full-access keys and that every key is
restricted to the account and the three request methods. It prints PASS or FAIL per check and an overall
result. `--skip-build` runs only the checks that need no Rust toolchain; `--wasm PATH` compares a wasm you
already have.

What it proves and does not: a match shows that this repository, built with the documented toolchain, produces
exactly the bytes the chain runs. The build is not reproducible by construction (`cargo near build
non-reproducible-wasm`), and the contract's on-chain metadata carries no build info (`"build_info": null`, which
the script prints), so source-to-bytecode provenance rests on the documented pins: `Cargo.lock` with `--locked`,
`near-sdk =5.29.1`, and rustc 1.94.1 (`DESIGN.md` does not pin cargo-near; the reproduction below used 0.23.0).
Use the pinned compiler.

One thing the wasm embeds is the absolute path of the cargo registry it was built under, so a different cargo
home changes the bytes on its own (measured: 430,168 bytes against the deployed 428,360). The script detects
that path in the deployed code and maps your cargo home onto it. Checked end to end: a clean build from the
committed tree reproduced the deployed hash with the default cargo home and with a different one, both with
rustc 1.94.1 and cargo-near 0.23.0. Other compiler or cargo-near versions have not been tried, and a mismatch
with a different toolchain is not by itself evidence that the deployed code differs.

The same claims are checked in your browser by the [live console](https://minder-production-28c77cab.up.railway.app/app) (source in `web/`): it reads the access keys
and the deployed code from public NEAR nodes and hashes the code locally; it reads the live policy and headroom;
it answers dry runs against the live contract; and for each cross-chain payment it recovers the signer from the
raw Sepolia transaction and shows it equals the contract's configured sender and the address Chain Signatures
derives for this contract.

## Live demo and links

- **Contract:** [`gate.minder-gate-owner-bf5fcd.testnet`](https://testnet.nearblocks.io/address/gate.minder-gate-owner-bf5fcd.testnet)
  on NEAR testnet. Its EVM sender on Sepolia is `0x73Cd4718d7577318D76Ab78db13a9Ec7CEcb7F3F`.
- **The live console and docs:** <https://minder-production-28c77cab.up.railway.app>. Attack the agent at
  [`/app`](https://minder-production-28c77cab.up.railway.app/app), read the contract's real limits, and check the
  recorded payments' signatures in your browser; the documentation is at
  [`/docs`](https://minder-production-28c77cab.up.railway.app/docs). Or run `web/` locally (see
  [Build and test](#build-and-test)): the page reads the live testnet contract straight from your browser, so a
  local run shows the same live data.
- **Design and evidence:** [`contract/DESIGN.md`](contract/DESIGN.md) is the source for the guarantees and figures above.
- **Security self-audit:** [`AUDIT.md`](AUDIT.md): scope and method, trust model, a threat-to-mechanism-to-test
  matrix, findings from our own tooling, known limitations and the proof ledger. It is the author's own review, not an
  independent audit.

A real run, recorded on testnet: payments the contract gated, signed through Chain Signatures and settled on
Sepolia.

| Payment | NEAR transaction (gated by the contract) | Sepolia transaction (settled) |
| --- | --- | --- |
| 0.05 NEAR to `vendor.minder-gate-owner-bf5fcd.testnet` | [`8UMiuKQN…2E1GRS`](https://testnet.nearblocks.io/txns/8UMiuKQNp9ExEe5AkQm4md16ps3k8wcYFk8Pc12E1GRS) | n/a (a NEAR transfer) |
| 0.0002 ETH to `0xE3390E9D6Fc9456B7E7cDf4678D918Dd4a2870d5` | [`8VkMZXPG…6Vw69U`](https://testnet.nearblocks.io/txns/8VkMZXPG7KPJSmykkXxtozLMQZc9E9RxgFTEnQ6Vw69U) | [`0xc7e3f81c…0fe368`](https://sepolia.etherscan.io/tx/0xc7e3f81c7b5a9550c08a8048c7132fcd72384329b47ec5ff7b0adda05a0fe368) |
| 0.5 USDC to `0xE3390E9D6Fc9456B7E7cDf4678D918Dd4a2870d5` | [`GW6eqnve…D8Y6gT`](https://testnet.nearblocks.io/txns/GW6eqnveLxSyB2UQEHUjUMUdLV2PZqGSCA3nZMD8Y6gT) | [`0xa0a38905…fe70c7`](https://sepolia.etherscan.io/tx/0xa0a389053b589b2ac355711e8072f0c51956fbdbbe7bd854e4ded2e038fe70c7) |

## Build and test

The contract (Rust 1.93 or newer; the documented results use rustc 1.94.1):

```
cd contract
cargo near build non-reproducible-wasm --locked   # the production wasm (cargo install --locked cargo-near)
./test.sh                                         # every test binary, with a per-binary summary
```

`contract/test.sh` runs the unit tests, the sandbox integration tests and the model-based harness. The first
run downloads the near-sandbox binary, and a full run takes about ten minutes (`DESIGN.md`, "Running").

The proofs (Kani installs its own nightly toolchain):

```
cargo install --locked kani-verifier && cargo kani setup     # once
cd contract/kani && cargo kani                               # all 43 harnesses
cd contract/kani && cargo kani --harness normalize_s_is_total_and_always_low_s   # one
```

The mutation tests (`cargo install --locked cargo-mutants`):

```
cd contract
cargo mutants -j 1 --timeout 120 --build-timeout 900 \
    --file src/policy.rs --file src/window.rs --file src/loosening.rs \
    --file src/timelock.rs --file src/gate.rs --file src/evm.rs -- --lib -- --skip model_tests
```

The console and relay (`web/`, Node 22.22 or newer):

```
cd web && npm ci && npm run build && npm start     # http://localhost:8080
npm run typecheck && npm test                      # the client and server tests
```

See [`web/README.md`](web/README.md) for the layout and for hosting it.

## Repository layout

```
contract/             the contract (Rust, near-sdk =5.29.1) and its evidence
  src/                policy.rs (the decision), loosening.rs (the timelock classifier), window.rs,
                      evm.rs, gate.rs (the EVM signing gate), timelock.rs, decision.rs, types.rs, lib.rs
  tests/              sandbox integration tests (near-workspaces), including the keyless deployment
  kani/               the Kani proofs, a crate of its own
  vectors/            viem reference vectors, their generators, and the 1,743-case corpus
  deploy/             deploy-keyless.sh: the keyless deployment and a key audit (testnet only)
  DESIGN.md           the design, the decisions and every result above
  test.sh             the canonical test command
test-contracts/       test doubles for a NEP-141 token and the Chain Signatures contract; never deployed
web/                  the live console, the documentation site and a small relay (see web/README.md)
scripts/              verify-deployment.sh
```

## Status

A hackathon project, built for the AI Security and Real-World Money tracks. **Testnet only**: it has never been
deployed to mainnet, it has not had an independent audit, and it should not be used with real funds. The
proofs and tests above are the project's own evidence, not a substitute for review.

## License

[MIT](LICENSE).
