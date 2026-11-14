/*
  The questions a first-time reader asks, answered from the design document, the contract, the live deployment,
  AUDIT.md and the README. One list, used twice: the docs show all of it, the landing page the questions marked
  `landing`. Each answer says what holds and what does not.
*/

import type { ReactNode } from 'react';
import { Link } from 'react-router';

import { ExternalLink } from './components/ExternalLink';
import { AUDIT_URL, CONTRACT_ID, EXPLORER_URL, README_URL, VERIFY_SCRIPT_URL } from './links';

export interface FaqItem {
  id: string;
  question: string;
  /** Whether the landing page shows it. */
  landing: boolean;
  answer: ReactNode;
}

export const FAQ: FaqItem[] = [
  {
    id: 'faq-jailbreak',
    question: "Can't the agent just be reprogrammed or jailbroken to bypass the firewall?",
    landing: true,
    answer: (
      <>
        <p>
          No, because the firewall is not in the agent. It is the contract&rsquo;s own decision function, and the agent
          holds only a function-call access key that the NEAR protocol restricts: the receiver is the contract, the
          methods are exactly <code>request_local_transfer</code>, <code>request_evm_payment</code> and{' '}
          <code>request_evm_token_payment</code>, and the key cannot transfer, deploy or add or delete keys. Anything
          else is refused before any contract code runs. A jailbroken agent can still <em>ask</em>, and every request meets
          the same policy.
        </p>
        <p>
          What it can still do is spend inside the limits, because the policy bounds the loss; it does not make spending
          impossible. See <Link to="/docs#agent-key">the agent key</Link> and the{' '}
          <Link to="/docs#non-guarantees">non-guarantees</Link>.
        </p>
      </>
    ),
  },
  {
    id: 'faq-stolen-key',
    question: "What if the agent's key is stolen?",
    landing: true,
    answer: (
      <>
        <p>
          A stolen key is the situation Minder assumes. The key is <em>narrow</em> (three methods on one
          contract), <em>revocable</em> (<code>revoke_mandate</code> flags the mandate, so the next call is denied,
          and deletes the key; the owner or the guardian can do it, and <code>freeze</code> stops every request at once) and{' '}
          <em>time-boxed</em> (the mandate expires). Until it is revoked, the loss is bounded by the per-transaction
          cap, the sliding window and the budget, plus the key&rsquo;s fixed 0.25 NEAR gas allowance.
        </p>
        <p>
          The honest limit: a payment already signed cannot be recalled (see below). The bounds are shown live on the{' '}
          <Link to="/app">console</Link>.
        </p>
      </>
    ),
  },
  {
    id: 'faq-deployed',
    question: 'Is this really deployed, or a mockup?',
    landing: true,
    answer: (
      <>
        <p>
          It is deployed on NEAR testnet as{' '}
          <ExternalLink href={EXPLORER_URL}>
            <code>{CONTRACT_ID}</code>
          </ExternalLink>
          , and a real run is recorded: a 0.05 NEAR payment, a 0.0002 ETH payment and a 0.5 USDC payment, each gated by the
          contract, signed through Chain Signatures and settled on Sepolia, with every transaction linked on the{' '}
          <Link to="/app">console</Link>. It is testnet only: it has never been deployed to mainnet.
        </p>
        <p>
          You do not have to take this page&rsquo;s word. The console reads the chain from your browser, and{' '}
          <ExternalLink href={VERIFY_SCRIPT_URL}>
            <code>scripts/verify-deployment.sh</code>
          </ExternalLink>{' '}
          builds the contract from a clean checkout, compares its hash with the deployed code, and checks the account&rsquo;s
          keys.
        </p>
      </>
    ),
  },
  {
    id: 'faq-keyless',
    question: 'What does "keyless" mean, and what is the catch?',
    landing: false,
    answer: (
      <>
        <p>
          The contract account holds <em>no full-access key</em>. It is created by one transaction (
          <code>CreateAccount</code>, <code>Transfer</code>, <code>DeployContract</code>, <code>init</code>) with no{' '}
          <code>AddKey</code>, so nobody, the deployer included, can sign as it, redeploy it or change its code. You can
          check the key list yourself: it should show only restricted keys (one while the agent&rsquo;s mandate is granted, none after a
          revoke) and nothing with full access.
        </p>
        <p>
          The catch, stated plainly: <em>there is no upgrade path</em>, so a bug is fixed by moving to a new account
          with the owner&rsquo;s recovery methods; and <em>the account that signs the deployment is the owner
          permanently</em>, since only it can call <code>init</code> and there is no way to change the owner. A multisig
          owner has to be the signer. See <Link to="/docs#keyless">the keyless deploy</Link>.
        </p>
      </>
    ),
  },
  {
    id: 'faq-ethereum',
    question: 'How can a NEAR contract move money on Ethereum without holding an EVM key?',
    landing: true,
    answer: (
      <>
        <p>
          Through NEAR Chain Signatures. The contract builds the Ethereum transaction itself, asks the signer contract to
          sign its hash, and gets a signature back. The signer derives the key from the calling account and a path, so the
          EVM address the contract controls is one only calls from this contract can request signatures for. The contract
          does not take the signature on faith: it checks that it recovers to its own configured address before it emits
          the transaction for anyone to broadcast.
        </p>
        <p>
          That relies on Chain Signatures being available and deriving keys as designed; the contract cannot prove that
          part. What it does prove, for real transactions, is shown on the <Link to="/app">console</Link>: the signer
          recovered from each signed Sepolia transaction equals the contract&rsquo;s configured sender and the address the
          signer derives for this contract. See <Link to="/docs#chain-signatures">Chain Signatures</Link>.
        </p>
      </>
    ),
  },
  {
    id: 'faq-malicious-owner',
    question: 'What if the owner is malicious?',
    landing: false,
    answer: (
      <>
        <p>
          Minder contains the agent, not the owner: the owner is the root and its fund recovery is outside the agent policy,
          so the recommended mitigation, a multisig owner, is external and not built here. Inside the contract there is a
          reaction window: any change that loosens authority (a higher cap, a new recipient, a longer mandate, higher fees)
          is timelocked, fund recovery is instant only to a pre-registered treasury, and the guardian can{' '}
          <code>freeze</code>, <code>revoke_mandate</code> or <code>cancel_change</code> at once. The guardian can only
          tighten, so a compromised guardian can at worst deny service.
        </p>
        <p>
          The limits: a hostile owner and guardian acting together can still drain after the delay, and in the demo
          deployment one operator holds both keys with a 120-second delay, so there the window is demonstrated, not an
          independent defence. See <Link to="/docs#timelock">the timelock</Link> and the{' '}
          <ExternalLink href={AUDIT_URL}>self-audit</ExternalLink>.
        </p>
      </>
    ),
  },
  {
    id: 'faq-undo',
    question: 'Can a payment be undone once it is signed?',
    landing: false,
    answer: (
      <>
        <p>
          No. A denied request is never signed, but a transaction the contract has signed is an ordinary EIP-1559
          transaction with no expiry, and anyone holding it can broadcast it. Freezing or revoking stops anything{' '}
          <em>new</em> from being signed, not what already exists. The loss is bounded by the limits that applied when it was
          signed, since it was reserved and booked then.
        </p>
        <p>
          The owner can try a manual recall by signing a replacement at the same nonce, which is a race against whoever
          broadcasts, so a mitigation and not a guarantee. This is the documented boundary called G2; see the{' '}
          <Link to="/docs#non-guarantees">non-guarantees</Link>.
        </p>
      </>
    ),
  },
  {
    id: 'faq-provable',
    question: 'Is "provable" real formal verification?',
    landing: true,
    answer: (
      <>
        <p>
          It is machine-checked within stated bounds, not complete formal verification. Kani does bounded model checking:
          43 harnesses prove properties of the firewall decision, the timelock classifier, the sliding window and
          signature normalization for <em>every</em> input within each harness&rsquo;s bounds. Alongside that sit mutation
          testing (241 of 247 viable mutants killed, the other 6 documented as equivalent) and a 1,743-case differential
          corpus against viem.
        </p>
        <p>
          The bounds are written down, and some things are not proved at all: the signed-transaction encoder&rsquo;s
          panic-freedom, the EVM recipient scope, and the reserve/settle accounting (covered by a stateful harness and tests
          instead). See <Link to="/docs#kani">the Kani proofs</Link> and the proof ledger in the{' '}
          <ExternalLink href={AUDIT_URL}>self-audit</ExternalLink>.
        </p>
      </>
    ),
  },
  {
    id: 'faq-own-agent',
    question: 'Can I use this for my own agent?',
    landing: true,
    answer: (
      <>
        <p>
          Yes: it is a reusable primitive. You deploy a keyless gate, set the policy and allowlists, grant a mandate to your
          agent&rsquo;s public key, and have the agent call <code>request_local_transfer</code>,{' '}
          <code>request_evm_payment</code> or <code>request_evm_token_payment</code> instead of ever holding funds. The
          steps are in the{' '}
          <ExternalLink href={README_URL}>README</ExternalLink>.
        </p>
        <p>
          Today it is testnet only, one mandate is active per contract (so one gate per agent), and there is no packaged
          client library: the calls are ordinary NEAR function calls.
        </p>
      </>
    ),
  },
  {
    id: 'faq-why-near',
    question: 'Why NEAR?',
    landing: false,
    answer: (
      <>
        <p>
          Two features of NEAR are what this design is built on. <em>Function-call access keys</em> let the
          protocol itself restrict the agent&rsquo;s key to named methods on one contract, so the restriction needs no custom
          code and holds before any contract logic runs. <em>Chain Signatures</em> lets a contract sign transactions
          for other chains without holding a key, so the same policy can gate money on Ethereum.
        </p>
        <p>
          That is why it is built here; this page does not claim it could not be built elsewhere.
        </p>
      </>
    ),
  },
];

/** The questions, as expandable rows that read as plain text with scripts off. */
export function FaqList({ items }: { items: FaqItem[] }) {
  return (
    <div className="faq">
      {items.map((item) => (
        <details key={item.id} className="faq__item" id={item.id}>
          <summary className="faq__q">{item.question}</summary>
          <div className="faq__a">{item.answer}</div>
        </details>
      ))}
    </div>
  );
}
