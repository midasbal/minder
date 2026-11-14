# Minder

A NEAR smart contract that acts as a policy firewall between an AI agent and money. The agent holds only
a narrow, revocable, time-boxed authority; the contract is the only path to funds and refuses any action
that breaks policy. Testnet only.

- `contract/`: the contract (Rust, near-sdk). Start with `contract/DESIGN.md`.
- `contract/deploy/`: the keyless deployment script (`deploy-keyless.sh`): the contract account is created with no full-access key,
  so nobody can act as it or change its code. Testnet only; see `contract/DESIGN.md`, "Keyless deployment".
- `test-contracts/`: test doubles used by the sandbox tests (a token and a signer). Never deployed.
- `spikes/`: throwaway experiments that informed the design.

## Run the tests

```
contract/test.sh
```

One command runs the whole suite: every test binary, even if one fails, with a per-binary summary and a
non-zero exit on any failure. It needs Rust 1.93 or newer and `cargo-near`
(`cargo install --locked cargo-near`). The first run downloads the near-sandbox binary, and a full run
takes about ten minutes. `contract/DESIGN.md` ("Running") explains the settings it relies on. The
bounded-model-checking proofs are separate: `cd contract/kani && cargo kani` (see "Formal verification"
in `DESIGN.md`).
