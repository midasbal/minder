import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { describe, it } from 'node:test';

import { attachField, layoutLattice, RADIUS, RELAX_MS, response, tangentAngle } from './field';

/* ---------- Pure physics ---------- */

describe('the lattice', () => {
  it('is a staggered field of dashes, not a square grid', () => {
    const { x, y, count } = layoutLattice(640, 400);
    const rows = new Map<number, number[]>();
    for (let i = 0; i < count; i += 1) rows.set(y[i]!, [...(rows.get(y[i]!) ?? []), x[i]!]);
    const [first, second] = [...rows.values()];
    assert.ok(rows.size > 4);
    assert.notEqual(first![0], second![0], 'alternate rows are shifted');
    assert.equal(Math.abs(second![0]! - first![0]!), 16, 'by half a step');
    const positions = new Set(Array.from({ length: count }, (_, i) => `${x[i]},${y[i]}`));
    assert.equal(positions.size, count, 'no two dashes share a place');
  });

  it('covers the whole viewport and is sparse', () => {
    const { x, y, count } = layoutLattice(1920, 1080);
    assert.ok(Math.max(...x) >= 1920 - 32 && Math.max(...y) >= 1080 - 28);
    assert.ok(count < 4000, `${count} dashes for a full-HD viewport`);
  });
});

describe('the response of the surface to the probe', () => {
  it('is full at the probe, nothing at the radius and beyond, and falls smoothly between', () => {
    assert.equal(response(0), 1);
    assert.equal(response(RADIUS), 0);
    assert.equal(response(RADIUS * 3), 0);
    let previous = 1;
    for (let d = 5; d < RADIUS; d += 5) {
      const now = response(d);
      assert.ok(now < previous && now > 0, `d=${d}`);
      previous = now;
    }
    assert.ok(response(RADIUS * 0.99) < 0.001, 'it eases to zero at the edge');
  });

  it('turns a dash onto the tangent of the circle around the probe', () => {
    assert.ok(Math.abs(Math.abs(tangentAngle(100, 0)) - Math.PI / 2) < 1e-9, 'right of the probe: vertical');
    assert.ok(Math.abs(tangentAngle(0, -100)) < 1e-9, 'above the probe: horizontal');
    for (let i = 0; i < 200; i += 1) {
      const angle = (i / 200) * 2 * Math.PI;
      const [dx, dy] = [Math.cos(angle) * 80, Math.sin(angle) * 80];
      const theta = tangentAngle(dx, dy);
      assert.ok(theta >= -Math.PI / 2 && theta < Math.PI / 2);
      const radial = Math.abs(Math.cos(theta) * dx + Math.sin(theta) * dy) / 80;
      assert.ok(radial < 1e-9, `perpendicular to the radius at ${angle.toFixed(2)}`);
    }
  });
});

/* ---------- The surface, against a fake window and a recording canvas ---------- */

interface Segment {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}

interface Stroke {
  style: string;
  segments: Segment[];
}

const YELLOW = 'rgba(255, 201, 60, 1)';

function fakeEnvironment({ fine = true, calm = true, width = 640, height = 400, ratio = 1 } = {}) {
  const media = new Map<string, { matches: boolean; listeners: Set<() => void> }>([
    ['(pointer: fine)', { matches: fine, listeners: new Set() }],
    ['(prefers-reduced-motion: no-preference)', { matches: calm, listeners: new Set() }],
  ]);
  const windowListeners = new Map<string, Set<(event: never) => void>>();
  const rootListeners = new Map<string, Set<() => void>>();
  const frames = new Map<number, (now: number) => void>();
  let nextFrame = 1;
  let clock = 0;
  const cancelled: number[] = [];

  const env = { width, height, ratio };
  const colours: Record<string, string> = { '--ink': '#e4e6da', '--live': '#ffc93c' };

  const win = {
    get innerWidth() {
      return env.width;
    },
    get innerHeight() {
      return env.height;
    },
    get devicePixelRatio() {
      return env.ratio;
    },
    getComputedStyle: () => ({ getPropertyValue: (name: string) => ` ${colours[name] ?? ''} ` }),
    matchMedia: (query: string) => {
      const entry = media.get(query)!;
      return {
        get matches() {
          return entry.matches;
        },
        addEventListener: (_type: string, listener: () => void) => entry.listeners.add(listener),
        removeEventListener: (_type: string, listener: () => void) => entry.listeners.delete(listener),
      };
    },
    addEventListener: (type: string, listener: (event: never) => void) => {
      windowListeners.set(type, (windowListeners.get(type) ?? new Set()).add(listener));
    },
    removeEventListener: (type: string, listener: (event: never) => void) => windowListeners.get(type)?.delete(listener),
    requestAnimationFrame: (callback: (now: number) => void) => {
      frames.set(nextFrame, callback);
      return nextFrame++;
    },
    cancelAnimationFrame: (id: number) => {
      cancelled.push(id);
      frames.delete(id);
    },
    document: {
      documentElement: {
        addEventListener: (type: string, listener: () => void) => {
          rootListeners.set(type, (rootListeners.get(type) ?? new Set()).add(listener));
        },
        removeEventListener: (type: string, listener: () => void) => rootListeners.get(type)?.delete(listener),
      },
    },
  } as unknown as Window;

  // The recording canvas: frames of strokes, each stroke its style and its segments.
  const drawn: Stroke[][] = [];
  const transforms: number[][] = [];
  let style = '';
  let current: Segment[] = [];
  let from = { x: 0, y: 0 };
  const context = {
    lineWidth: 0,
    lineCap: '',
    get strokeStyle() {
      return style;
    },
    set strokeStyle(value: string) {
      style = value;
    },
    setTransform: (...values: number[]) => transforms.push(values),
    clearRect: () => drawn.push([]),
    beginPath: () => {
      current = [];
    },
    moveTo: (x: number, y: number) => {
      from = { x, y };
    },
    lineTo: (x: number, y: number) => {
      current.push({ x1: from.x, y1: from.y, x2: x, y2: y });
      from = { x, y };
    },
    stroke: () => drawn[drawn.length - 1]!.push({ style, segments: current }),
  };
  const canvas = { width: 0, height: 0, getContext: () => context } as unknown as HTMLCanvasElement;

  return {
    win,
    canvas,
    env,
    drawn,
    transforms,
    cancelled,
    move: (clientX: number, clientY: number) => {
      for (const listener of windowListeners.get('pointermove') ?? []) listener({ clientX, clientY } as never);
    },
    leave: () => rootListeners.get('pointerleave')?.forEach((listener) => listener()),
    resize: () => windowListeners.get('resize')?.forEach((listener) => listener(undefined as never)),
    /** Runs the waiting frame callbacks at the next 16.7 ms tick. */
    tick: () => {
      clock += 16.7;
      const pending = [...frames.values()];
      frames.clear();
      pending.forEach((callback) => callback(clock));
      return pending.length;
    },
    /** Ticks until no frame is waiting, and says how many ticks that took. */
    settle: (limit = 500) => {
      let ticks = 0;
      while (frames.size > 0 && ticks < limit) {
        ticks += 1;
        clock += 16.7;
        const pending = [...frames.values()];
        frames.clear();
        pending.forEach((callback) => callback(clock));
      }
      return ticks;
    },
    pending: () => frames.size,
    listeners: () => (windowListeners.get('pointermove')?.size ?? 0) + (rootListeners.get('pointerleave')?.size ?? 0),
    resizeListeners: () => windowListeners.get('resize')?.size ?? 0,
    setMedia: (query: string, matches: boolean) => {
      const entry = media.get(query)!;
      entry.matches = matches;
      entry.listeners.forEach((listener) => listener());
    },
  };
}

const inkStrokes = (frame: Stroke[]) => frame.filter((stroke) => stroke.style !== YELLOW);
const yellowStrokes = (frame: Stroke[]) => frame.filter((stroke) => stroke.style === YELLOW);
const segmentsOf = (strokes: Stroke[]) => strokes.reduce((total, stroke) => total + stroke.segments.length, 0);
const length = (s: Segment) => Math.hypot(s.x2 - s.x1, s.y2 - s.y1);
const alpha = (stroke: Stroke) => Number(/, ([\d.]+)\)$/.exec(stroke.style)![1]);

describe('the still plane', () => {
  it('is drawn once at rest: every dash, one stroke, no yellow, and no frame waiting', () => {
    const env = fakeEnvironment();
    attachField(env.canvas, env.win);
    const { count } = layoutLattice(640, 400);
    assert.equal(env.drawn.length, 1);
    assert.equal(env.drawn[0]!.length, 1);
    assert.equal(env.drawn[0]![0]!.segments.length, count);
    assert.equal(alpha(env.drawn[0]![0]!), 0.1, 'near-invisible');
    assert.equal(yellowStrokes(env.drawn[0]!).length, 0);
    assert.equal(env.pending(), 0, 'nothing runs while idle');
  });

  it('draws at rest as horizontal dashes', () => {
    const env = fakeEnvironment();
    attachField(env.canvas, env.win);
    for (const s of env.drawn[0]![0]!.segments) assert.equal(s.y1, s.y2);
  });

  it('is all there is for a coarse pointer, or when reduced motion is asked for', () => {
    for (const options of [{ fine: false }, { calm: false }]) {
      const env = fakeEnvironment(options);
      attachField(env.canvas, env.win);
      assert.equal(env.listeners(), 0, JSON.stringify(options));
      env.move(100, 100);
      assert.equal(env.pending(), 0);
      assert.equal(env.drawn.length, 1, 'drawn once and never again');
      assert.equal(yellowStrokes(env.drawn[0]!).length, 0);
    }
  });

  it('is drawn crisply for the screen it is on', () => {
    const env = fakeEnvironment({ width: 500, height: 300, ratio: 2 });
    attachField(env.canvas, env.win);
    assert.equal(env.canvas.width, 1000);
    assert.equal(env.canvas.height, 600);
    assert.deepEqual(env.transforms[0], [2, 0, 0, 2, 0, 0]);
  });
});

describe('the surface under the probe', () => {
  it('asks for one frame however many events arrive, and writes nothing inside an event', () => {
    const env = fakeEnvironment();
    attachField(env.canvas, env.win);
    const before = env.drawn.length;
    for (let i = 0; i < 300; i += 1) env.move(100 + i, 100 + (i % 50));
    assert.equal(env.pending(), 1);
    assert.equal(env.drawn.length, before, 'nothing is drawn inside an event');
    env.tick();
    assert.equal(env.drawn.length, before + 1, 'one redraw for the frame');
  });

  it('draws every dash exactly once, and the crosshair as the only yellow', () => {
    const env = fakeEnvironment();
    attachField(env.canvas, env.win);
    const { count } = layoutLattice(640, 400);
    env.move(320, 200);
    env.tick();
    const frame = env.drawn[env.drawn.length - 1]!;
    assert.equal(segmentsOf(inkStrokes(frame)), count, 'no dash is missing or doubled');
    const yellow = yellowStrokes(frame);
    assert.equal(yellow.length, 1);
    assert.equal(yellow[0]!.segments.length, 4, 'a crosshair: four arms');
    // The arms are centred on the pointer, with a gap, horizontal and vertical.
    const [left, right, up, down] = yellow[0]!.segments as [Segment, Segment, Segment, Segment];
    assert.ok(left.y1 === left.y2 && right.y1 === right.y2 && up.x1 === up.x2 && down.x1 === down.x2);
    assert.ok(Math.abs((left.x2 + right.x1) / 2 - 320.5) < 1, 'centred on the pointer');
    assert.ok(right.x1 - left.x2 > 0, 'with a gap at the centre');
  });

  it('turns dashes onto the field around the probe, longer and brighter the closer they are', () => {
    const env = fakeEnvironment();
    attachField(env.canvas, env.win);
    const probe = { x: 320, y: 200 };
    env.move(probe.x, probe.y);
    env.tick();
    const frame = env.drawn[env.drawn.length - 1]!;
    const active = inkStrokes(frame)
      .filter((stroke) => stroke.segments.length < layoutLattice(640, 400).count)
      .flatMap((stroke) => stroke.segments.map((segment) => ({ segment, alpha: alpha(stroke) })));
    const measured = active.map(({ segment, alpha: a }) => {
      const cx = (segment.x1 + segment.x2) / 2 - probe.x;
      const cy = (segment.y1 + segment.y2) / 2 - probe.y;
      return { distance: Math.hypot(cx, cy), length: length(segment), alpha: a, segment, cx, cy };
    });
    const near = measured.filter((m) => m.distance < RADIUS * 0.3);
    const far = measured.filter((m) => m.distance > RADIUS * 0.85);
    assert.ok(near.length > 5 && far.length > 5, `${near.length} near, ${far.length} far`);
    assert.ok(Math.min(...near.map((m) => m.length)) > Math.max(...far.map((m) => m.length)), 'longer near');
    assert.ok(Math.min(...near.map((m) => m.alpha)) > Math.max(...far.map((m) => m.alpha)), 'brighter near');
    for (const m of near) {
      const along = Math.abs((m.segment.x2 - m.segment.x1) * m.cx + (m.segment.y2 - m.segment.y1) * m.cy) / (m.length * m.distance);
      assert.ok(along < 0.6, `tangential, not radial (${along.toFixed(2)})`);
    }
  });

  it('pushes dashes outward from the probe, never inward', () => {
    const env = fakeEnvironment();
    attachField(env.canvas, env.win);
    const rest = layoutLattice(640, 400);
    env.move(320, 200);
    env.tick();
    const frame = env.drawn[env.drawn.length - 1]!;
    const centres = inkStrokes(frame).flatMap((s) => s.segments.map((g) => [(g.x1 + g.x2) / 2, (g.y1 + g.y2) / 2] as const));
    // Every resting position that has a dash nearby (within the push) moved away or stayed.
    let pushed = 0;
    for (let i = 0; i < rest.count; i += 1) {
      const d0 = Math.hypot(rest.x[i]! - 320, rest.y[i]! - 200);
      if (d0 > RADIUS * 0.5 || d0 < 10) continue;
      const nearest = centres.reduce((best, c) => (Math.hypot(c[0] - rest.x[i]!, c[1] - rest.y[i]!) < Math.hypot(best[0] - rest.x[i]!, best[1] - rest.y[i]!) ? c : best));
      const d1 = Math.hypot(nearest[0] - 320, nearest[1] - 200);
      assert.ok(d1 >= d0 - 0.75, 'not pulled in');
      if (d1 > d0 + 0.5) pushed += 1;
    }
    assert.ok(pushed > 0, 'some dashes are displaced outward');
  });

  it('keeps the loop alive only while something changes, then goes fully idle', () => {
    const env = fakeEnvironment();
    attachField(env.canvas, env.win);
    env.move(320, 200);
    const ticks = env.settle();
    assert.ok(ticks >= 1 && ticks < 200, `settled after ${ticks} frames`);
    assert.equal(env.pending(), 0, 'no frame waits once it has settled');
    const after = env.drawn.length;
    env.tick();
    assert.equal(env.drawn.length, after, 'and nothing is drawn');
    // A still pointer keeps the field lit and the crosshair drawn.
    const last = env.drawn[env.drawn.length - 1]!;
    assert.equal(yellowStrokes(last).length, 1);
  });

  it('leaves a brief wake: a dash relaxes over about a quarter of a second, not at once', () => {
    const env = fakeEnvironment();
    attachField(env.canvas, env.win);
    env.move(100, 100);
    env.tick();
    env.move(500, 300);
    env.tick();
    const frame = env.drawn[env.drawn.length - 1]!;
    const nearOld = inkStrokes(frame).flatMap((s) => s.segments.map((g) => ({ g, a: alpha(s) })));
    const touched = nearOld.filter(({ g }) => Math.hypot((g.x1 + g.x2) / 2 - 100, (g.y1 + g.y2) / 2 - 100) < 60);
    assert.ok(touched.some(({ a }) => a > 0.1), 'the old position is still registering one frame later');
    const ticks = env.settle();
    assert.ok(ticks * 16.7 > RELAX_MS * 0.5, 'and takes a while to relax');
  });

  it('returns to the still plane when the pointer leaves: no yellow, every dash at rest', () => {
    const env = fakeEnvironment();
    attachField(env.canvas, env.win);
    const { count } = layoutLattice(640, 400);
    env.move(320, 200);
    env.settle();
    env.leave();
    env.settle();
    const last = env.drawn[env.drawn.length - 1]!;
    assert.equal(yellowStrokes(last).length, 0);
    assert.equal(last.length, 1, 'one stroke: all dashes at rest');
    assert.equal(last[0]!.segments.length, count);
    assert.equal(env.pending(), 0);
  });

  it('uses yellow only for the crosshair, and neither verdict colour', () => {
    const env = fakeEnvironment();
    attachField(env.canvas, env.win);
    for (const [x, y] of [[50, 50], [320, 200], [600, 380], [10, 390]] as const) {
      env.move(x, y);
      env.tick();
    }
    env.settle();
    for (const frame of env.drawn) {
      for (const stroke of frame) {
        if (stroke.style === YELLOW) assert.equal(stroke.segments.length, 4, 'yellow is the crosshair');
        else assert.match(stroke.style, /^rgba\(228, 230, 218, /, 'everything else is the ink colour');
      }
    }
  });
});

describe('following the settings and the window', () => {
  it('follows the pointer and motion settings as they change', () => {
    const env = fakeEnvironment({ calm: false });
    attachField(env.canvas, env.win);
    assert.equal(env.listeners(), 0);

    env.setMedia('(prefers-reduced-motion: no-preference)', true);
    assert.equal(env.listeners(), 2, 'switched on');
    env.move(300, 200);
    assert.equal(env.pending(), 1);

    const before = env.drawn.length;
    env.setMedia('(pointer: fine)', false);
    assert.equal(env.listeners(), 0, 'switched off');
    assert.equal(env.cancelled.length, 1, 'the waiting frame was cancelled');
    assert.equal(env.drawn.length, before + 1, 'the still plane is drawn again');
    assert.equal(yellowStrokes(env.drawn[env.drawn.length - 1]!).length, 0);
  });

  it('redraws for a new window size, once per frame however many resize events arrive', () => {
    const env = fakeEnvironment();
    attachField(env.canvas, env.win);
    const small = layoutLattice(640, 400).count;
    env.env.width = 1280;
    env.env.height = 800;
    for (let i = 0; i < 20; i += 1) env.resize();
    assert.equal(env.pending(), 1);
    env.tick();
    assert.equal(env.canvas.width, 1280);
    const last = env.drawn[env.drawn.length - 1]!;
    assert.equal(last[0]!.segments.length, layoutLattice(1280, 800).count);
    assert.ok(last[0]!.segments.length > small);
    assert.equal(env.pending(), 0);
  });

  it('detaches everything', () => {
    const env = fakeEnvironment();
    const detach = attachField(env.canvas, env.win);
    env.move(10, 10);
    env.resize();
    detach();
    assert.equal(env.listeners(), 0);
    assert.equal(env.resizeListeners(), 0);
    assert.equal(env.pending(), 0);
    assert.equal(env.cancelled.length, 2, 'the waiting frames were cancelled');
  });
});

describe('what the effect is not', () => {
  const source = readFileSync(new URL('./field.ts', import.meta.url), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');

  it('uses no blur, glow, gradient, noise or particles, and no colour of its own', () => {
    for (const banned of [
      'createRadialGradient',
      'createLinearGradient',
      'createConicGradient',
      'shadowBlur',
      'filter',
      'blur',
      'Math.random',
      'globalCompositeOperation',
    ]) {
      assert.ok(!source.includes(banned), banned);
    }
    assert.ok(!/#[0-9a-fA-F]{3,8}\b/.test(source), 'no colour literal: the colours come from the page tokens');
    assert.ok(source.includes("'--ink'") && source.includes("'--live'"));
    assert.ok(!/--(allow|deny)/.test(source), 'the verdict colours are not touched');
  });
});
