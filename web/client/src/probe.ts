/**
 * The cursor probe: carries the pointer's position into two CSS custom
 * properties, `--mx` and `--my`, on one element, for the grid to light up around.
 *
 * An event stores the latest coordinates and, if no frame is already waiting,
 * asks for one; the frame writes both properties once. So a burst of events costs
 * a single pair of writes, and nothing here reads layout. The probe runs only for
 * a fine pointer (a mouse or a pen) and only when the user has not asked for
 * reduced motion; it follows both settings as they change.
 *
 * Returns a function that detaches everything.
 */
export function attachProbe(target: HTMLElement, win: Window): () => void {
  const finePointer = win.matchMedia('(pointer: fine)');
  const motionOk = win.matchMedia('(prefers-reduced-motion: no-preference)');
  const root = win.document.documentElement;

  let x = 0;
  let y = 0;
  let frame = 0;
  let listening = false;

  const write = () => {
    frame = 0;
    target.style.setProperty('--mx', `${x}px`);
    target.style.setProperty('--my', `${y}px`);
  };

  const onMove = (event: PointerEvent) => {
    x = event.clientX;
    y = event.clientY;
    if (target.dataset.probe !== 'on') target.dataset.probe = 'on';
    if (frame === 0) frame = win.requestAnimationFrame(write);
  };

  const onLeave = () => {
    target.dataset.probe = 'off';
  };

  const stop = () => {
    win.removeEventListener('pointermove', onMove);
    root.removeEventListener('pointerleave', onLeave);
    if (frame !== 0) win.cancelAnimationFrame(frame);
    frame = 0;
    target.dataset.probe = 'off';
    listening = false;
  };

  const sync = () => {
    const wanted = finePointer.matches && motionOk.matches;
    if (wanted && !listening) {
      win.addEventListener('pointermove', onMove, { passive: true });
      root.addEventListener('pointerleave', onLeave);
      listening = true;
    } else if (!wanted && listening) {
      stop();
    }
  };

  sync();
  finePointer.addEventListener('change', sync);
  motionOk.addEventListener('change', sync);

  return () => {
    finePointer.removeEventListener('change', sync);
    motionOk.removeEventListener('change', sync);
    stop();
  };
}
