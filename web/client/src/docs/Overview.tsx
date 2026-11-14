import { DocSection } from './parts';

export function Overview() {
  return (
    <DocSection id="overview" number="1" title="Overview">
      <p>
        Minder is a policy firewall between an AI agent and money. The agent holds only a narrow, revocable,
        time-boxed authority; the contract is the only path to funds.
      </p>
      <p>
        <strong>The thesis:</strong> assume the agent is already compromised, contain what it can do, and prove the
        containment.
      </p>
      <p>
        Minder is a NEAR smart contract that holds the funds an agent may spend and gates every spend with one
        deterministic policy. It is a spending gate for autonomous agents in two places: on NEAR, where it moves
        native NEAR and NEP-141 tokens; and cross-chain through NEAR Chain Signatures, where it signs EVM
        transactions (native ETH and ERC-20 transfers) for its own address, and only when the policy allows. This
        deployment runs on NEAR testnet and Sepolia.
      </p>
      <h3 className="doc-h3 doc-h3--plain">Two things set it apart</h3>
      <ul>
        <li>
          <strong>It gates the signature itself.</strong> The contract is the sole signer for its own EVM address. A
          payment the policy denies is never signed.
        </li>
        <li>
          <strong>Provable, not asserted.</strong> The policy core is formally proved with Kani (within stated
          bounds), mutation-tested, and checked against a differential corpus built from an independent
          implementation, viem.
        </li>
      </ul>
      <p>
        The rest of this page says who is trusted, what is guaranteed and what is not, how the system works, and how
        each claim is checked.
      </p>
    </DocSection>
  );
}
