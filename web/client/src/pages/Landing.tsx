import { Link } from 'react-router';

import { LinkButton } from '../components/Buttons';
import { Datasheet } from '../components/Datasheet';
import { PolicyGate } from '../components/PolicyGate';
import { FAQ, FaqList } from '../faq';

/** The guarantees as datasheets. Static values: the footnotes say how to verify them live. */
function Guarantees() {
  return (
    <div className="sheets">
      <Datasheet
        id="sheet-custody"
        title="CUSTODY"
        tag="keyless"
        rows={[
          { key: 'full-access keys on account', value: '0', verified: true },
          { key: 'agent key authority', value: '3 methods only' },
          { key: 'governance', value: 'external owner' },
          { key: 'policy loosening', value: 'timelocked' },
        ]}
        footnote="Static values. Verify live: view_access_key_list + view_code hash"
      />
      <Datasheet
        id="sheet-verification"
        title="VERIFICATION"
        tag="provable"
        rows={[
          { key: 'kani proofs', value: '43 · bounded' },
          { key: 'mutations caught', value: '200+' },
          { key: 'differential corpus', value: '1,743 EVM encodings' },
          { key: 'keyless deploy', value: 'verified', verified: true },
        ]}
        footnote={<Link to="/docs#proofs">Guarantees hold within stated bounds</Link>}
      />
    </div>
  );
}

export function Landing() {
  return (
    <div className="landing">
      <section className="hero">
        <div className="hero__copy">
          <h1 className="hero__title">A policy firewall between an AI agent and money.</h1>
          <p className="hero__sub">
            Give an autonomous agent a wallet and the question is no longer if it gets compromised, but what it can do
            when it does. <strong>Minder assumes the agent is already compromised</strong>, contains what it can spend,
            and proves the containment.
          </p>
          <div className="cta-row">
            <LinkButton to="/app" variant="primary">
              Open the live firewall
            </LinkButton>
            <LinkButton to="/docs" variant="secondary">
              Read the docs
            </LinkButton>
          </div>
        </div>
        <PolicyGate />
      </section>

      <section className="section" aria-labelledby="threat-model">
        <p className="section__label">01 · THREAT MODEL</p>
        <h2 className="section__title" id="threat-model">
          The agent will be compromised. Plan for it.
        </h2>
        <p className="section__lead">
          Prompt injection, a poisoned tool, a jailbreak: any of them can turn an agent against its owner. Minder does
          not try to keep the agent clean. It holds the agent at arm&rsquo;s length behind a single narrow key, routes
          every movement of money through one deterministic gate, and makes the contract the only path to funds.
          Containment, not trust.
        </p>
      </section>

      <section className="section" aria-labelledby="guarantees">
        <p className="section__label">02 · WHAT HOLDS, AND WHY YOU CAN CHECK IT</p>
        <h2 className="section__title" id="guarantees">
          Two guarantees that don&rsquo;t rest on trusting us.
        </h2>
        <Guarantees />
      </section>

      <section className="section" aria-labelledby="cross-chain">
        <p className="section__label">03 · CROSS-CHAIN</p>
        <h2 className="section__title" id="cross-chain">
          It gates the signature itself.
        </h2>
        <p className="section__lead">
          Through NEAR Chain Signatures the contract is the sole signer for its own EVM address. A cross-chain payment
          is never approved off-chain and relayed: the firewall decides whether the transaction gets signed at all. The
          output is a real EIP-1559 transaction, broadcast to Sepolia.
        </p>
      </section>

      <section className="section" aria-labelledby="questions">
        <p className="section__label">04 · QUESTIONS</p>
        <h2 className="section__title" id="questions">
          The questions you are about to ask.
        </h2>
        <FaqList items={FAQ.filter((item) => item.landing)} />
        <p className="section__lead">
          More, including what the catch of &ldquo;keyless&rdquo; is and what happens if the owner is malicious, in the{' '}
          <Link to="/docs#faq">docs FAQ</Link>.
        </p>
      </section>

      <section className="closing" aria-labelledby="closing">
        <h2 className="closing__title" id="closing">
          See it refuse.
        </h2>
        <p className="closing__text">Drive the compromised agent yourself and watch the firewall hold.</p>
        <div className="cta-row">
          <LinkButton to="/app" variant="primary">
            Open the live firewall
          </LinkButton>
          <LinkButton to="/docs" variant="secondary">
            Read the docs
          </LinkButton>
        </div>
      </section>
    </div>
  );
}
