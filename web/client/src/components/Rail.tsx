import { motion } from 'framer-motion';

import { draw } from '../motion';

/** The policy boundary: the one line between the agent and the money. Decorative. */
export function Rail() {
  return (
    <div className="rail" aria-hidden="true">
      <div className="rail__ticks" />
      <motion.div className="rail__line" variants={draw} />
      <span className="rail__label">Policy boundary</span>
    </div>
  );
}
