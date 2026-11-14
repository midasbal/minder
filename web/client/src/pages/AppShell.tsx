import { motion } from 'framer-motion';

import { Rail } from '../components/Rail';
import { rise, stagger } from '../motion';

/** The interactive surface. A placeholder: the two panels are reserved, nothing is wired. */
export function AppShell() {
  return (
    <div className="page-wide">
      <motion.div variants={stagger} initial="hidden" animate="shown">
        <motion.p className="label" variants={rise}>
          Live demo
        </motion.p>
        <motion.h1 className="page-title" variants={rise}>
          The agent, the boundary, the money.
        </motion.h1>
        <motion.p className="lede" variants={rise}>
          This surface is a placeholder. The scenarios and the live status will appear in the two panels below.
        </motion.p>
      </motion.div>

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

        <Rail />

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
    </div>
  );
}
