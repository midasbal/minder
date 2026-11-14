import { motion } from 'framer-motion';
import { Link } from 'react-router';

import { rise, stagger } from '../motion';
import { Architecture } from './Architecture';
import { Faq } from './Faq';
import { LiveDemo } from './LiveDemo';
import { Overview } from './Overview';
import { Proofs } from './Proofs';
import { Reference } from './Reference';
import { ThreatModel } from './ThreatModel';
import { ALL_IDS, TOC } from './toc';
import { useActiveSection } from './useActiveSection';

export function Docs() {
  const active = useActiveSection(ALL_IDS);

  return (
    <div className="docs">
      <aside className="docs__nav">
        <nav aria-label="On this page">
          <p className="label">On this page</p>
          <ul className="toc">
            {TOC.map((entry) => (
              <li key={entry.id}>
                <Link to={`/docs#${entry.id}`} aria-current={active === entry.id ? 'location' : undefined}>
                  {entry.title}
                </Link>
                {entry.children && (
                  <ul>
                    {entry.children.map((child) => (
                      <li key={child.id}>
                        <Link to={`/docs#${child.id}`} aria-current={active === child.id ? 'location' : undefined}>
                          {child.title}
                        </Link>
                      </li>
                    ))}
                  </ul>
                )}
              </li>
            ))}
          </ul>
        </nav>
      </aside>

      <article className="docs__body">
        <motion.header variants={stagger} initial="hidden" animate="shown">
          <motion.p className="label" variants={rise}>
            Documentation
          </motion.p>
          <motion.h1 className="page-title" variants={rise}>
            What Minder guarantees, and how that is checked.
          </motion.h1>
        </motion.header>

        <Overview />
        <ThreatModel />
        <Architecture />
        <Proofs />
        <LiveDemo />
        <Faq />
        <Reference />
      </article>
    </div>
  );
}
