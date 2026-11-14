import { motion, MotionConfig, type Variants } from 'framer-motion';

import { useRelayHealth, type RelayStatus } from './useRelayHealth';

const EASE = [0.22, 1, 0.36, 1] as const;

const stagger: Variants = {
  hidden: {},
  shown: { transition: { staggerChildren: 0.12, delayChildren: 0.15 } },
};

const rise: Variants = {
  hidden: { opacity: 0, y: 20 },
  shown: { opacity: 1, y: 0, transition: { duration: 0.8, ease: EASE } },
};

// Grows from the origin set in CSS: the top of the vertical rail, the left of the horizontal one.
const draw: Variants = {
  hidden: { scale: 0 },
  shown: { scale: 1, transition: { duration: 1.2, ease: EASE, delay: 0.3 } },
};

const RELAY_LABEL: Record<RelayStatus, string> = {
  checking: 'relay: checking',
  online: 'relay: online',
  offline: 'relay: offline',
};

export function App() {
  const relay = useRelayHealth();

  return (
    <MotionConfig reducedMotion="user">
      <div className="page">
        <header className="masthead">
          <span className="wordmark">
            <span className="wordmark__bar" aria-hidden="true" />
            Minder
          </span>
          <span className="tag">Testnet only</span>
        </header>

        <main className="main">
          <motion.h1 className="headline" variants={stagger} initial="hidden" animate="shown">
            <motion.span className="headline__line" variants={rise}>
              A policy firewall between an AI agent and money.
            </motion.span>
            <motion.em className="headline__line headline__line--accent" variants={rise}>
              Assume the agent is already compromised.
            </motion.em>
          </motion.h1>

          <motion.section
            className="stage"
            aria-label="The agent and the money, with the policy boundary between them"
            variants={stagger}
            initial="hidden"
            animate="shown"
          >
            <motion.div className="zone zone--agent" variants={rise}>
              <p className="label">01 / Agent side</p>
              <h2 className="zone__title">The agent</h2>
              <p className="zone__text">
                Untrusted. It holds one narrow, revocable, time-boxed key, and nothing else.
              </p>
              <div className="slot">
                <p className="slot__name">Agent control surface</p>
                <p className="slot__note">Reserved. The scenarios the agent can attempt will live here.</p>
              </div>
            </motion.div>

            <div className="rail" aria-hidden="true">
              <div className="rail__ticks" />
              <motion.div className="rail__line" variants={draw} />
              <span className="rail__label">Policy boundary</span>
            </div>

            <motion.div className="zone zone--money" variants={rise}>
              <p className="label">02 / Money side</p>
              <h2 className="zone__title">The contract</h2>
              <p className="zone__text">
                The only path to funds. Every request meets the same deterministic policy.
              </p>
              <div className="slot">
                <p className="slot__name">Status panel</p>
                <p className="slot__note">Reserved. Live policy, balances and decisions will show here.</p>
              </div>
            </motion.div>
          </motion.section>
        </main>

        <footer className="footer">
          <span>Minder</span>
          <span className="chip" data-state={relay} role="status">
            <span className="chip__dot" aria-hidden="true" />
            {RELAY_LABEL[relay]}
          </span>
        </footer>
      </div>
    </MotionConfig>
  );
}
