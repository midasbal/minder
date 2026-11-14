import type { ReactNode } from 'react';
import { Link } from 'react-router';

import { ExternalLink } from './ExternalLink';

type Variant = 'primary' | 'ghost';

export function LinkButton({ to, variant, children }: { to: string; variant: Variant; children: ReactNode }) {
  return (
    <Link to={to} className={`btn btn--${variant}`}>
      {children}
    </Link>
  );
}

export function ExternalButton({ href, variant, children }: { href: string; variant: Variant; children: ReactNode }) {
  return (
    <ExternalLink href={href} className={`btn btn--${variant}`}>
      {children}
    </ExternalLink>
  );
}
