import { useEffect } from 'react';
import { useLocation } from 'react-router';

/**
 * A route change starts at the top of the page; a link with a hash (a docs
 * section) scrolls to that element. The router does neither by itself.
 */
export function ScrollManager() {
  const { pathname, hash } = useLocation();

  useEffect(() => {
    const reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    const behavior = reduce ? 'auto' : 'smooth';
    if (hash) {
      const target = document.getElementById(decodeURIComponent(hash.slice(1)));
      if (target) {
        target.scrollIntoView({ behavior, block: 'start' });
        return;
      }
    }
    window.scrollTo({ top: 0, behavior: 'auto' });
  }, [pathname, hash]);

  return null;
}
