import { Link } from 'react-router';

import { ExternalLink } from '../components/ExternalLink';
import { AUDIT_URL, VERIFY_SCRIPT_URL } from '../links';
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
          <strong>Checked, not asserted.</strong> The policy core is machine-checked with Kani bounded model checking
          (within stated bounds, which are documented), mutation-tested, and checked against a differential corpus built
          from an independent implementation, viem. This is not a claim of complete formal verification.
        </li>
      </ul>
      <p>
        The rest of this page says who is trusted, what is guaranteed and what is not, how the system works, and how
        each claim is checked.
      </p>
      <h3 className="doc-h3 doc-h3--plain">Check it yourself</h3>
      <ul>
        <li>
          <Link to="/app">The live console</Link> attacks the deployed contract with dry runs, reads its limits from the
          chain in your browser, and checks who signed each payment of a recorded real run.
        </li>
        <li>
          <ExternalLink href={VERIFY_SCRIPT_URL}>
            <code>scripts/verify-deployment.sh</code>
          </ExternalLink>{' '}
          builds the contract from a clean checkout, compares its hash with the deployed code, and checks the
          account&rsquo;s keys.
        </li>
        <li>
          <ExternalLink href={AUDIT_URL}>
            <code>AUDIT.md</code>
          </ExternalLink>{' '}
          is the author&rsquo;s security review: scope, trust model, a threat-to-test matrix, findings and limits. It is a
          self-audit, not an independent one.
        </li>
      </ul>
    </DocSection>
  );
}
