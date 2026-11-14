import { useEffect, useRef } from 'react';

import { attachProbe } from '../probe';

/**
 * The engineering grid behind every page: a dim base grid, and a lit copy of it
 * (with a soft glow) that the cursor probe reveals around the pointer. The probe
 * itself is in `probe.ts`.
 */
export function GridBackground() {
  const layer = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const element = layer.current;
    if (!element) return;
    return attachProbe(element, window);
  }, []);

  return (
    <div className="grid-bg" ref={layer} data-probe="off" aria-hidden="true">
      <div className="grid-bg__base" />
      <div className="grid-bg__lit" />
      <div className="grid-bg__glow" />
    </div>
  );
}
