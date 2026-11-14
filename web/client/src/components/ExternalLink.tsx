import type { AnchorHTMLAttributes } from 'react';

type Props = Omit<AnchorHTMLAttributes<HTMLAnchorElement>, 'target' | 'rel'>;

/** A link that leaves the site: opens in a new tab, says so to screen readers. */
export function ExternalLink({ children, ...rest }: Props) {
  return (
    <a {...rest} target="_blank" rel="noopener noreferrer">
      {children}
      <span className="ext" aria-hidden="true">
        {' '}
        ↗
      </span>
      <span className="sr-only"> (opens in a new tab)</span>
    </a>
  );
}
