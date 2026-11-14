import { Link, NavLink } from 'react-router';

import { REPO_URL } from '../links';
import { ExternalLink } from './ExternalLink';

export function Header() {
  return (
    <header className="site-header">
      <div className="site-header__inner">
        <Link to="/" className="wordmark" aria-label="Minder, home">
          <span className="wordmark__bar" aria-hidden="true" />
          Minder
        </Link>

        <nav className="nav" aria-label="Primary">
          <NavLink to="/" end>
            Overview
          </NavLink>
          <NavLink to="/app">Live demo</NavLink>
          <NavLink to="/docs">Docs</NavLink>
          <ExternalLink href={REPO_URL}>Source</ExternalLink>
        </nav>

        <span className="tag">Testnet only</span>
      </div>
    </header>
  );
}
