import { motion } from 'framer-motion';

import { AgentPanel } from '../components/console/AgentPanel';
import { LivePanel } from '../components/console/LivePanel';
import { useConsole } from '../components/console/useConsole';
import { FreezeDrill } from '../components/console/FreezeDrill';
import { RealRun } from '../components/console/RealRun';
import { RedTeam } from '../components/console/RedTeam';
import { Rail } from '../components/Rail';
import { LIVE_RUN } from '../chain/liverun';
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
          This agent is compromised and holds a key to real money.
        </motion.h1>
        <motion.p className="lede" variants={rise}>
          The firewall decides what that key can do. Every payment is gated by the contract, and the most the agent can lose is
          bounded. Attack it below, read the contract's real limits, then check payments that really moved.
        </motion.p>
        <motion.ul className="tracks" variants={rise}>
          <li>
            <span className="tracks__name">AI Security</span>
            <span className="tracks__what">Bounded loss from a compromised agent: it can spend only inside the limits, and nothing else.</span>
          </li>
          <li>
            <span className="tracks__name">Real-World Money</span>
            <span className="tracks__what">Real USDC and ETH payments, gated, signed with no master key, and settled on Sepolia.</span>
          </li>
        </motion.ul>
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

      <RedTeam />

      <RealRun run={LIVE_RUN} state={state} store={store} />

      <FreezeDrill />
    </div>
  );
}
