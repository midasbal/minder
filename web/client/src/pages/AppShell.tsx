import { motion } from 'framer-motion';

import { AgentPanel } from '../components/console/AgentPanel';
import { LivePanel } from '../components/console/LivePanel';
import { useConsole } from '../components/console/useConsole';
import { Rail } from '../components/Rail';
import { rise, stagger } from '../motion';

/** The live console: the agent's controls on one side of the boundary, the contract's state on the other. */
export function AppShell() {
  const { store, state } = useConsole();
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
          Act as a compromised agent against the live contract on one side; read the contract's real state from the chain on the other.
          Every verdict is the contract's own answer, and nothing on this page is simulated.
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
          <AgentPanel store={store} state={state} />
        </motion.div>

        <Rail />

        <motion.div className="zone zone--money" variants={rise}>
          <p className="label">02 / Money side</p>
          <h2 className="zone__title">The contract</h2>
          <p className="zone__text">
            The only path to funds. Every request meets the same deterministic policy.
          </p>
          <LivePanel store={store} state={state} />
        </motion.div>
      </motion.section>
    </div>
  );
}
