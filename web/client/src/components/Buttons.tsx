import type { ReactNode } from 'react';
import { Link } from 'react-router';

type Variant = 'primary' | 'secondary';

export function LinkButton({ to, variant, children }: { to: string; variant: Variant; children: ReactNode }) {
  return (
    <Link to={to} className={`btn btn--${variant}`}>
      {children}
    </Link>
  );
}
