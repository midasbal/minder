/*
  What the console knows: the last read of the chain, and the last dry run.

  One store, shared by the two panels and kept for the whole visit, so going to another page and back does
  not read the chain again. The chain is read on first load, when the visitor asks, and never on a timer.
*/

import { dryRun, type DryRunInput, type DryRunResult } from './contract';
import { RpcFailure, createRpc, type Rpc } from './rpc';
import { readSnapshot, type Snapshot } from './snapshot';

export type RunState =
  | { phase: 'idle' }
  | { phase: 'running'; input: DryRunInput }
  | { phase: 'done'; input: DryRunInput; result: DryRunResult }
  | { phase: 'failed'; input: DryRunInput; message: string };

export interface ConsoleState {
  /** `loading` with a snapshot present is a refresh: the old reading stays until the new one lands. */
  reading: 'idle' | 'loading' | 'ready';
  snapshot: Snapshot | null;
  run: RunState;
}

export interface ConsoleStore {
  getState(): ConsoleState;
  subscribe(listener: () => void): () => void;
  /** Reads the chain unless it has already been read; `force` reads again (the Refresh button). */
  load(force?: boolean): Promise<void>;
  dryRun(input: DryRunInput): Promise<void>;
}

/**
 * Public NEAR testnet nodes that answer cross-origin browser requests, in order of preference. FastNEAR answers in
 * about 0.1 s; the NEAR-hosted node adds a flat 3 s to every call (measured), so it is the fallback.
 */
export const NEAR_RPC_ENDPOINTS = ['https://rpc.testnet.fastnear.com', 'https://rpc.testnet.near.org'] as const;

const IDLE: ConsoleState = { reading: 'idle', snapshot: null, run: { phase: 'idle' } };

export function createConsoleStore(rpc: Rpc, now: () => number = Date.now): ConsoleStore {
  let state: ConsoleState = IDLE;
  const listeners = new Set<() => void>();
  let loading: Promise<void> | null = null;

  const set = (next: Partial<ConsoleState>) => {
    state = { ...state, ...next };
    listeners.forEach((listener) => listener());
  };

  return {
    getState: () => state,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    load(force = false) {
      if (loading) return loading;
      if (state.reading === 'ready' && !force) return Promise.resolve();
      set({ reading: 'loading' });
      loading = readSnapshot(rpc, { now })
        .then((snapshot) => set({ reading: 'ready', snapshot }))
        .finally(() => {
          loading = null;
        });
      return loading;
    },
    async dryRun(input) {
      if (state.run.phase === 'running') return;
      set({ run: { phase: 'running', input } });
      try {
        set({ run: { phase: 'done', input, result: await dryRun(rpc, input) } });
      } catch (error) {
        const message = error instanceof RpcFailure ? error.message : `Unexpected answer from the chain (${(error as Error).message})`;
        set({ run: { phase: 'failed', input, message } });
      }
    },
  };
}

let shared: ConsoleStore | null = null;

/** The store the page uses, over the real public nodes. */
export function sharedConsoleStore(): ConsoleStore {
  shared ??= createConsoleStore(createRpc({ endpoints: NEAR_RPC_ENDPOINTS }));
  return shared;
}

/** What a server render sees: nothing read yet. */
export const SERVER_STATE: ConsoleState = IDLE;
