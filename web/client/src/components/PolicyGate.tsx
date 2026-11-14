import { motion, useReducedMotion, type Variants } from 'framer-motion';

import { CONTRACT_ID } from '../links';
import { EASE } from '../motion';

type Verdict = 'ALLOW' | 'DENY';

/**
 * Illustrative decisions against the deployed NEAR policy (0.50 per transaction,
 * 1.00 per 60-second window, 2.00 budget, one allowed recipient). Static: nothing
 * here is read from the chain.
 */
const POLICY: ReadonlyArray<readonly [string, string]> = [
  ['per-tx', '0.50'],
  ['window', '1.00 / 60s'],
  ['budget', '2.00'],
  ['allowlist', '1'],
];

const DECISIONS: ReadonlyArray<{ request: string; reason: string; verdict: Verdict }> = [
  { request: 'transfer 0.40 → vendor.near', reason: 'within every limit', verdict: 'ALLOW' },
  { request: 'transfer 0.60 → vendor.near', reason: 'exceeds per-tx cap 0.50', verdict: 'DENY' },
  { request: 'transfer 0.30 → unknown.near', reason: 'recipient not on allowlist', verdict: 'DENY' },
];

const panel: Variants = {
  hidden: { opacity: 0, y: 14 },
  shown: {
    opacity: 1,
    y: 0,
    transition: { duration: 0.7, ease: EASE, delayChildren: 0.45, staggerChildren: 0.28 },
  },
};

const decision: Variants = {
  hidden: { opacity: 0, x: -8 },
  shown: { opacity: 1, x: 0, transition: { duration: 0.5, ease: EASE } },
};

export function PolicyGate() {
  const reduce = useReducedMotion();

  return (
    <div className="gate-wrap">
      <motion.aside
        className="gate"
        aria-label="The policy gate: an agent's requests, and the verdict on each"
        variants={panel}
        initial={reduce ? false : 'hidden'}
        animate="shown"
      >
        <div className="gate__bar">
          <span className="gate__title">
            <span className="live-dot" aria-hidden="true" />
            POLICY GATE
          </span>
          <span className="gate__id">{CONTRACT_ID}</span>
        </div>

        <div className="gate__body">
          <div className="gate__col">
            <p className="gate__label">AGENT · untrusted</p>
            <div className="node">
              <p className="node__title">fn-call key</p>
              <p className="node__sub">3 methods · time-boxed · revocable</p>
            </div>
          </div>

          <div className="gate__policy">
            <p className="gate__label">POLICY</p>
            <dl className="kv">
              {POLICY.map(([key, value]) => (
                <div className="kv__row" key={key}>
                  <dt>{key}</dt>
                  <dd>{value}</dd>
                </div>
              ))}
            </dl>
          </div>

          <div className="gate__col">
            <p className="gate__label">FUNDS</p>
            <div className="node">
              <p className="node__title">treasury</p>
              <p className="node__sub">NEAR + EVM via chain signatures</p>
            </div>
          </div>
        </div>

        <ul className="decisions">
          {DECISIONS.map(({ request, reason, verdict }) => (
            <motion.li className="decision" key={request} variants={decision}>
              <div className="decision__text">
                <p className="decision__request">{request}</p>
                <p className="decision__reason">{reason}</p>
              </div>
              <span className={`stamp stamp--${verdict === 'ALLOW' ? 'allow' : 'deny'}`}>{verdict}</span>
            </motion.li>
          ))}
        </ul>
      </motion.aside>
      <p className="gate-wrap__caption">Illustrative decisions on the deployed policy. Static.</p>
    </div>
  );
}
