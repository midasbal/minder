/*
  The sensing surface: the page background as a still, precise plane that registers
  the cursor like an instrument.

  At rest it is a sparse, staggered lattice of short dashes, almost invisible. Within
  a radius of the pointer each dash turns onto the tangent of the circle around the
  probe (a field around a point), lengthens, brightens and is pushed slightly outward,
  with every effect falling off smoothly to nothing at the edge of the radius. A dash
  the probe has touched relaxes back to rest over a fraction of a second, so the probe
  leaves a brief wake.

  Hairlines only: no blur, no gradients, no glow. The colour is the page's own ink
  token, read from CSS.

  Cost: nothing while idle. A frame is requested only when the pointer moves or a
  dash is still relaxing, and the loop ends as soon as nothing changes. For a touch or
  coarse pointer, or for reduced motion, only the still plane is drawn, once (and again
  on resize).
*/

const SPACING_X = 32;
const ROW_HEIGHT = 28;

/** How far the probe reaches, in CSS pixels. */
export const RADIUS = 190;

const REST_LENGTH = 5;
const MAX_LENGTH = 15;
const REST_ALPHA = 0.1;
const MAX_ALPHA = 0.3;
/** The furthest a dash is pushed away from the probe, in CSS pixels. */
const PUSH = 7;
/** Time constant of a dash relaxing back to rest, in milliseconds. */
export const RELAX_MS = 240;
/** Below this a dash counts as at rest. */
const AT_REST = 0.012;

/** How many levels of opacity the active dashes are drawn in (one stroke per level). */
const ALPHA_LEVELS = 8;

export interface Lattice {
  count: number;
  x: Float32Array;
  y: Float32Array;
}

/** The dashes' resting positions: rows that alternate by half a step, so it reads as texture, not graph paper. */
export function layoutLattice(width: number, height: number): Lattice {
  const columns = Math.ceil(width / SPACING_X) + 1;
  const rows = Math.ceil(height / ROW_HEIGHT) + 1;
  const x = new Float32Array(columns * rows);
  const y = new Float32Array(columns * rows);
  let n = 0;
  for (let row = 0; row < rows; row += 1) {
    const shift = row % 2 === 0 ? 0 : SPACING_X / 2;
    for (let column = 0; column < columns; column += 1) {
      x[n] = column * SPACING_X + shift + SPACING_X / 4;
      y[n] = row * ROW_HEIGHT + ROW_HEIGHT / 2;
      n += 1;
    }
  }
  return { count: n, x, y };
}

/** How strongly the probe acts at a distance: 1 at the probe, smoothly 0 at the radius and beyond. */
export function response(distance: number): number {
  const t = Math.min(Math.max(1 - distance / RADIUS, 0), 1);
  return t * t * (3 - 2 * t);
}

/**
 * The orientation of a dash at offset (dx, dy) from the probe, along the tangent
 * of the circle around it, as an undirected angle in [-pi/2, pi/2).
 */
export function tangentAngle(dx: number, dy: number): number {
  const angle = Math.atan2(dy, dx) + Math.PI / 2;
  return ((((angle + Math.PI / 2) % Math.PI) + Math.PI) % Math.PI) - Math.PI / 2;
}

type Rgb = readonly [number, number, number];

function readColour(win: Window, name: string): Rgb {
  const value = win.getComputedStyle(win.document.documentElement).getPropertyValue(name).trim();
  const hex = /^#([0-9a-f]{6})$/i.exec(value);
  if (!hex) throw new Error(`${name} must be a six-digit hex colour, found "${value}"`);
  const n = parseInt(hex[1]!, 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

const rgba = ([r, g, b]: Rgb, alpha: number) => `rgba(${r}, ${g}, ${b}, ${alpha})`;

/**
 * Draws the surface on a canvas and follows the pointer. Returns a function that
 * detaches everything.
 */
export function attachField(canvas: HTMLCanvasElement, win: Window): () => void {
  const context = canvas.getContext('2d');
  if (!context) throw new Error('a 2d canvas is not available');

  const ink = readColour(win, '--ink');
  const finePointer = win.matchMedia('(pointer: fine)');
  const motionOk = win.matchMedia('(prefers-reduced-motion: no-preference)');
  const root = win.document.documentElement;

  let width = 0;
  let height = 0;
  let ratio = 1;
  let lattice = layoutLattice(0, 0);
  // Per dash: how disturbed it is (0 at rest), the angle it was turned to, and the
  // direction away from the probe it was pushed along.
  let energy = new Float32Array(0);
  let turn = new Float32Array(0);
  let awayX = new Float32Array(0);
  let awayY = new Float32Array(0);

  let pointerX = 0;
  let pointerY = 0;
  let present = false;
  let moved = false;
  let frame = 0;
  let resizeFrame = 0;
  let lastTime = -1;
  let listening = false;

  /** Rounds a coordinate to the centre of a device pixel line, so a 1px line is crisp. */
  const snap = (value: number) => {
    const odd = Math.round(ratio) % 2 === 1;
    const offset = odd ? 0.5 : 0;
    return (Math.round(value * ratio - offset) + offset) / ratio;
  };

  const resize = () => {
    width = win.innerWidth;
    height = win.innerHeight;
    ratio = win.devicePixelRatio || 1;
    canvas.width = Math.round(width * ratio);
    canvas.height = Math.round(height * ratio);
    lattice = layoutLattice(width, height);
    energy = new Float32Array(lattice.count);
    turn = new Float32Array(lattice.count);
    awayX = new Float32Array(lattice.count);
    awayY = new Float32Array(lattice.count);
  };

  const render = () => {
    context.setTransform(ratio, 0, 0, ratio, 0, 0);
    context.clearRect(0, 0, width, height);
    context.lineWidth = 1;
    context.lineCap = 'butt';

    // Every dash at rest, in one stroke.
    context.strokeStyle = rgba(ink, REST_ALPHA);
    context.beginPath();
    for (let i = 0; i < lattice.count; i += 1) {
      if (energy[i]! > 0) continue;
      const y = snap(lattice.y[i]!);
      context.moveTo(snap(lattice.x[i]! - REST_LENGTH / 2), y);
      context.lineTo(snap(lattice.x[i]! + REST_LENGTH / 2), y);
    }
    context.stroke();

    // The disturbed dashes, one stroke per opacity level.
    const levels: number[][] = Array.from({ length: ALPHA_LEVELS + 1 }, () => []);
    for (let i = 0; i < lattice.count; i += 1) {
      const s = energy[i]!;
      if (s <= 0) continue;
      const alpha = REST_ALPHA + (MAX_ALPHA - REST_ALPHA) * s;
      levels[Math.round((alpha / MAX_ALPHA) * ALPHA_LEVELS)]!.push(i);
    }
    levels.forEach((members, level) => {
      if (members.length === 0) return;
      context.strokeStyle = rgba(ink, (level / ALPHA_LEVELS) * MAX_ALPHA);
      context.beginPath();
      for (const i of members) {
        const s = energy[i]!;
        const length = REST_LENGTH + (MAX_LENGTH - REST_LENGTH) * s;
        const push = PUSH * s * s;
        const cx = lattice.x[i]! + awayX[i]! * push;
        const cy = lattice.y[i]! + awayY[i]! * push;
        const half = length / 2;
        const dx = Math.cos(turn[i]! * s) * half;
        const dy = Math.sin(turn[i]! * s) * half;
        context.moveTo(cx - dx, cy - dy);
        context.lineTo(cx + dx, cy + dy);
      }
      context.stroke();
    });
  };

  /** Advances every dash by `elapsed` ms. Returns whether anything changed. */
  const update = (elapsed: number): boolean => {
    const relax = Math.exp(-elapsed / RELAX_MS);
    let changed = moved;
    moved = false;
    for (let i = 0; i < lattice.count; i += 1) {
      const before = energy[i]!;
      const relaxed = before * relax;
      let driven = 0;
      let dx = 0;
      let dy = 0;
      let distance = 0;
      if (present) {
        dx = lattice.x[i]! - pointerX;
        dy = lattice.y[i]! - pointerY;
        distance = Math.hypot(dx, dy);
        if (distance < RADIUS) driven = response(distance);
      }
      let after: number;
      if (driven > 0 && driven >= relaxed) {
        after = driven;
        turn[i] = tangentAngle(dx, dy);
        awayX[i] = distance > 0 ? dx / distance : 0;
        awayY[i] = distance > 0 ? dy / distance : 0;
      } else {
        after = relaxed < AT_REST ? 0 : relaxed;
      }
      if (Math.abs(after - before) > 1e-4) changed = true;
      energy[i] = after;
    }
    return changed;
  };

  const tick = (now: number) => {
    frame = 0;
    const elapsed = lastTime < 0 ? 16.7 : Math.min(now - lastTime, 50);
    lastTime = now;
    const keepGoing = update(elapsed);
    render();
    if (keepGoing) frame = win.requestAnimationFrame(tick);
    else lastTime = -1;
  };

  const wake = () => {
    if (frame === 0) frame = win.requestAnimationFrame(tick);
  };

  const onMove = (event: PointerEvent) => {
    pointerX = event.clientX;
    pointerY = event.clientY;
    present = true;
    moved = true;
    wake();
  };

  const onLeave = () => {
    present = false;
    wake();
  };

  const onResize = () => {
    if (resizeFrame !== 0) return;
    resizeFrame = win.requestAnimationFrame(() => {
      resizeFrame = 0;
      resize();
      render();
    });
  };

  const stopFollowing = () => {
    win.removeEventListener('pointermove', onMove);
    root.removeEventListener('pointerleave', onLeave);
    if (frame !== 0) win.cancelAnimationFrame(frame);
    frame = 0;
    lastTime = -1;
    present = false;
    moved = false;
    energy.fill(0);
    listening = false;
  };

  const sync = () => {
    const wanted = finePointer.matches && motionOk.matches;
    if (wanted && !listening) {
      win.addEventListener('pointermove', onMove, { passive: true });
      root.addEventListener('pointerleave', onLeave);
      listening = true;
    } else if (!wanted && listening) {
      stopFollowing();
      render();
    }
  };

  resize();
  render();
  sync();
  win.addEventListener('resize', onResize);
  finePointer.addEventListener('change', sync);
  motionOk.addEventListener('change', sync);

  return () => {
    win.removeEventListener('resize', onResize);
    finePointer.removeEventListener('change', sync);
    motionOk.removeEventListener('change', sync);
    if (resizeFrame !== 0) win.cancelAnimationFrame(resizeFrame);
    resizeFrame = 0;
    stopFollowing();
  };
}
