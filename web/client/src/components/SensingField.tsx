import { useEffect, useRef } from 'react';

import { attachField } from '../field';

/**
 * The page background: a still plane that registers the cursor. The drawing and
 * the pointer handling are in `field.ts`.
 */
export function SensingField() {
  const canvas = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const element = canvas.current;
    if (!element) return;
    return attachField(element, window);
  }, []);

  return <canvas className="field" ref={canvas} aria-hidden="true" />;
}
