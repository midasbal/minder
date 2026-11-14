import { motion } from 'framer-motion';
import type { ReactNode } from 'react';
import { Link } from 'react-router';

import { ExternalButton, LinkButton } from '../components/Buttons';
import { Rail } from '../components/Rail';
import { REPO_URL } from '../links';
import { reveal, rise, riseAfterHeadline, stagger } from '../motion';

function Chapter({ index, title, children }: { index: string; title: string; children: ReactNode }) {
  return (
    <motion.section className="chapter" {...reveal}>
      <p className="chapter__index label">{index}</p>
      <div className="chapter__body">
        <h2 className="chapter__title">{title}</h2>
        {children}
      </div>
    </motion.section>
  );
}

function Hero() {
  return (
    <section className="hero">
      <motion.h1 className="headline" variants={stagger} initial="hidden" animate="shown">
        <motion.span className="headline__line" variants={rise}>
          A policy firewall between an AI agent and money.
        </motion.span>
        <motion.em className="headline__line headline__line--accent" variants={rise}>
          Assume the agent is already compromised.
        </motion.em>
      </motion.h1>

      <motion.div className="cta-row" variants={riseAfterHeadline} initial="hidden" animate="shown">
        <LinkButton to="/app" variant="primary">
          Try the live firewall
        </LinkButton>
        <LinkButton to="/docs" variant="ghost">
          Read the docs
        </LinkButton>
        <ExternalButton href={REPO_URL} variant="ghost">
          View source
        </ExternalButton>
      </motion.div>
    </section>
  );
}

/** The agent, the policy boundary, the contract: the same split as the app, with what each side is. */
function Containment() {
  return (
    <motion.div
      className="stage stage--flow"
      aria-label="The agent, the policy boundary, and the contract"
      variants={stagger}
      initial="hidden"
      whileInView="shown"
      viewport={{ once: true, amount: 0.25 }}
    >
      <motion.div className="zone zone--agent" variants={rise}>
        <p className="label">Agent side</p>
        <h3 className="zone__title">The agent</h3>
        <p className="zone__text">One function-call key, and nothing else. It can call three methods:</p>
        <ul className="idents">
          <li>
            <code>request_local_transfer</code>
          </li>
          <li>
            <code>request_evm_payment</code>
          </li>
          <li>
            <code>request_evm_token_payment</code>
          </li>
        </ul>
      </motion.div>

      <Rail />

      <motion.div className="zone zone--money" variants={rise}>
        <p className="label">Money side</p>
        <h3 className="zone__title">The contract</h3>
        <p className="zone__text">The only path to funds. Every request meets the same policy:</p>
        <ul className="idents idents--plain">
          <li>Per-transaction cap</li>
          <li>Sliding-window limit</li>
          <li>Lifetime budget</li>
          <li>Recipient allowlists</li>
          <li>Asset scope</li>
        </ul>
      </motion.div>
    </motion.div>
  );
}

function Stats() {
  return (
    <ul className="stats">
      <li className="stat">
        <span className="stat__value">43</span>
        <span className="stat__label">Kani proofs</span>
        <span className="stat__note">
          18 on the encoder, the window and the firewall decision; 25 on the timelock classifier.
        </span>
      </li>
      <li className="stat">
        <span className="stat__value">
          241<span className="stat__of">/247</span>
        </span>
        <span className="stat__label">viable mutants killed</span>
        <span className="stat__note">
          259 generated. The other 6 viable ones are documented equivalents that cannot change behaviour.
        </span>
      </li>
      <li className="stat">
        <span className="stat__value">1,743</span>
        <span className="stat__label">differential cases</span>
        <span className="stat__note">The encoder compared byte for byte with an independent implementation, viem.</span>
      </li>
    </ul>
  );
}

function SigningPath() {
  const steps = [
    ['Request', 'The agent asks for a payment on another chain.'],
    ['Decision', 'The policy decides, on chain, before anything else happens.'],
    ['Signature', 'Only if it allows: the contract asks Chain Signatures to sign a transaction it built itself.'],
    ['Broadcast', 'The signed transaction goes to Sepolia.'],
  ] as const;
  return (
    <ol className="path">
      {steps.map(([name, text], index) => (
        <li className="path__step" key={name}>
          <span className="path__index">{String(index + 1).padStart(2, '0')}</span>
          <span className="path__name">{name}</span>
          <span className="path__text">{text}</span>
        </li>
      ))}
    </ol>
  );
}

export function Landing() {
  return (
    <div className="landing">
      <Hero />

      <Chapter index="01" title="The agent will be compromised. Plan for it.">
        <p className="prose">
          AI agents are getting wallets. Prompt injection, a poisoned tool, a jailbreak: any of them can turn the
          agent against its owner. Minder does not try to keep the agent clean. It assumes the agent is already
          compromised, contains what it can do, and proves the containment.
        </p>
      </Chapter>

      <Chapter index="02" title="One narrow key. One deterministic gate.">
        <p className="prose">
          The agent holds a single function-call key: narrow, revocable, time-boxed, scoped to three payment methods
          and nothing else. It never touches the funds. The contract is the only path to money, and every request,
          local or cross-chain, meets the same deterministic policy before a cent moves.
        </p>
        <Containment />
      </Chapter>

      <Chapter index="03" title="The contract has no master key.">
        <p className="prose">
          Minder&rsquo;s account is deployed with zero full-access keys. There is no master key on the account to
          phish, leak, or misuse. Governance lives in a separate owner account and is bound by the contract itself: it
          can tighten or freeze instantly, but every policy loosening must clear a public timelock. This is structural, not a
          setting, and the keyless account is verifiable live on chain.
        </p>
        <div className="badge" role="group" aria-label="Full-access keys on the contract account: 0. Static for now.">
          <span className="badge__count">0</span>
          <span className="badge__label">full-access keys</span>
          <span className="badge__note">Static for now. This will read live from chain later.</span>
        </div>
      </Chapter>

      <Chapter index="04" title="We prove the limits, not just test them.">
        <p className="prose">
          The limit logic is not &ldquo;trust us.&rdquo; Its core is formally verified with Kani&rsquo;s bounded model
          checking, hardened against 200+ code mutations, and cross-checked against a differential corpus of EVM
          encodings. The guarantees hold within stated bounds, not merely across the cases a test suite happened to
          try.
        </p>
        <Stats />
        <p className="fineprint">
          The proofs hold within documented bounds.{' '}
          <Link to="/docs#proofs">See exactly what is and is not proved.</Link>
        </p>
      </Chapter>

      <Chapter index="05" title="It gates the signature itself.">
        <p className="prose">
          Through NEAR Chain Signatures, the contract is the sole signer for its own EVM address. A cross-chain
          payment is not approved off-chain and relayed; the firewall decides whether the transaction gets signed at
          all. Real signatures, live on Sepolia.
        </p>
        <SigningPath />
      </Chapter>

      <motion.section className="closing" {...reveal}>
        <h2 className="closing__title">See it refuse.</h2>
        <p className="closing__text">Drive the compromised agent yourself and watch the firewall hold.</p>
        <div className="cta-row">
          <LinkButton to="/app" variant="primary">
            Try the live firewall
          </LinkButton>
          <LinkButton to="/docs" variant="ghost">
            Read the docs
          </LinkButton>
        </div>
      </motion.section>
    </div>
  );
}
