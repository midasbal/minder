import { Link } from 'react-router';

import { EXPLORER_URL, REPO_URL } from '../links';
import { useRelayHealth, type RelayStatus } from '../useRelayHealth';
import { ExternalLink } from './ExternalLink';

const RELAY_LABEL: Record<RelayStatus, string> = {
  checking: 'relay: checking',
  online: 'relay: online',
  offline: 'relay: offline',
};

export function Footer() {
  const relay = useRelayHealth();

  return (
    <footer className="site-footer">
      <div className="site-footer__inner">
        <div className="site-footer__brand">
          <Link to="/" className="wordmark wordmark--small" aria-label="Minder, home">
            <span className="wordmark__bar" aria-hidden="true" />
            Minder
          </Link>
          <p className="site-footer__line">NEAR testnet</p>
        </div>

        <nav className="site-footer__links" aria-label="Footer">
          <Link to="/docs">Docs</Link>
          <ExternalLink href={REPO_URL}>Source</ExternalLink>
          <ExternalLink href={EXPLORER_URL}>Contract on explorer</ExternalLink>
        </nav>

        <span className="chip" data-state={relay} role="status">
          <span className={relay === 'online' ? 'chip__dot live-dot' : 'chip__dot'} aria-hidden="true" />
          {RELAY_LABEL[relay]}
        </span>
      </div>
    </footer>
  );
}
