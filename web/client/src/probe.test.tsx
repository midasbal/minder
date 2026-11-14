import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { attachProbe } from './probe';

/** A window with just enough in it for the probe, and a manual animation-frame queue. */
function fakeEnvironment({ fine = true, calm = true } = {}) {
  const media = new Map<string, { matches: boolean; listeners: Set<() => void> }>([
    ['(pointer: fine)', { matches: fine, listeners: new Set() }],
    ['(prefers-reduced-motion: no-preference)', { matches: calm, listeners: new Set() }],
  ]);
  const windowListeners = new Map<string, Set<(event: never) => void>>();
  const rootListeners = new Map<string, Set<() => void>>();
  const frames = new Map<number, () => void>();
  let nextFrame = 1;
  const cancelled: number[] = [];

  const win = {
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
    requestAnimationFrame: (callback: () => void) => {
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

  const writes: Array<[string, string]> = [];
  const target = {
    style: { setProperty: (name: string, value: string) => writes.push([name, value]) },
    dataset: { probe: 'off' } as DOMStringMap,
  } as unknown as HTMLElement;

  return {
    win,
    target,
    writes,
    cancelled,
    move: (clientX: number, clientY: number) => {
      for (const listener of windowListeners.get('pointermove') ?? []) listener({ clientX, clientY } as never);
    },
    leave: () => rootListeners.get('pointerleave')?.forEach((listener) => listener()),
    flush: () => {
      const pending = [...frames.values()];
      frames.clear();
      pending.forEach((callback) => callback());
    },
    listeners: () => (windowListeners.get('pointermove')?.size ?? 0) + (rootListeners.get('pointerleave')?.size ?? 0),
    pending: () => frames.size,
    setMedia: (query: string, matches: boolean) => {
      const entry = media.get(query)!;
      entry.matches = matches;
      entry.listeners.forEach((listener) => listener());
    },
  };
}

describe('the cursor probe', () => {
  it('writes the custom properties once per frame, however many events arrive', () => {
    const env = fakeEnvironment();
    attachProbe(env.target, env.win);

    for (let i = 0; i < 200; i += 1) env.move(i, i * 2);
    assert.equal(env.writes.length, 0, 'no write happens inside an event');
    assert.equal(env.pending(), 1, 'one frame is waiting, not 200');

    env.flush();
    assert.deepEqual(env.writes, [
      ['--mx', '199px'],
      ['--my', '398px'],
    ]);

    env.move(10, 20);
    env.flush();
    assert.deepEqual(env.writes.slice(2), [
      ['--mx', '10px'],
      ['--my', '20px'],
    ]);
  });

  it('turns the probe on at the first move and off when the pointer leaves', () => {
    const env = fakeEnvironment();
    attachProbe(env.target, env.win);
    assert.equal(env.target.dataset.probe, 'off');
    env.move(5, 5);
    assert.equal(env.target.dataset.probe, 'on');
    env.leave();
    assert.equal(env.target.dataset.probe, 'off');
  });

  it('does nothing for a coarse pointer, or when reduced motion is asked for', () => {
    for (const options of [{ fine: false }, { calm: false }]) {
      const env = fakeEnvironment(options);
      attachProbe(env.target, env.win);
      assert.equal(env.listeners(), 0, JSON.stringify(options));
      env.move(1, 1);
      env.flush();
      assert.equal(env.writes.length, 0, JSON.stringify(options));
      assert.equal(env.target.dataset.probe, 'off');
    }
  });

  it('follows the settings as they change', () => {
    const env = fakeEnvironment({ calm: false });
    attachProbe(env.target, env.win);
    assert.equal(env.listeners(), 0);

    env.setMedia('(prefers-reduced-motion: no-preference)', true);
    assert.equal(env.listeners(), 2, 'switched on');
    env.move(3, 4);
    assert.equal(env.pending(), 1);

    env.setMedia('(pointer: fine)', false);
    assert.equal(env.listeners(), 0, 'switched off');
    assert.equal(env.cancelled.length, 1, 'the waiting frame was cancelled');
    assert.equal(env.target.dataset.probe, 'off');
    env.flush();
    assert.equal(env.writes.length, 0, 'and nothing was written');
  });

  it('detaches everything', () => {
    const env = fakeEnvironment();
    const detach = attachProbe(env.target, env.win);
    env.move(1, 1);
    detach();
    assert.equal(env.listeners(), 0);
    assert.equal(env.cancelled.length, 1);
    env.move(2, 2);
    assert.equal(env.pending(), 0);
  });
});
