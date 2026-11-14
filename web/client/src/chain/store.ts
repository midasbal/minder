/*
  What the console knows: the last read of the chain, and the last dry run.

  One store, shared by the two panels and kept for the whole visit, so going to another page and back does
  not read the chain again. The chain is read on first load, when the visitor asks, and never on a timer.
*/

import { dryRun, type DryRunInput, type DryRunResult } from './contract';
import { LIVE_RUN, type LiveRun } from './liverun';
import { proveProvenance, type Proof } from './provenance';
import { RpcFailure, createJsonRpc, createRpc, type JsonRpc, type Rpc } from './rpc';
import { SEPOLIA_RPC_ENDPOINTS, confirmSettlement, type Settlement } from './settlement';
import { readSnapshot, type Snapshot } from './snapshot';

export type RunState =
  | { phase: 'idle' }
  | { phase: 'running'; input: DryRunInput }
  | { phase: 'done'; input: DryRunInput; result: DryRunResult }
  | { phase: 'failed'; input: DryRunInput; message: string };

export type ProofState = { phase: 'idle' } | { phase: 'running'; previous: Proof | null } | { phase: 'done'; proof: Proof };

export type SettlementState = { phase: 'idle' } | { phase: 'running' } | { phase: 'done'; byHash: Record<string, Settlement> };

export interface ConsoleState {
  /** `loading` with a snapshot present is a refresh: the old reading stays until the new one lands. */
  reading: 'idle' | 'loading' | 'ready';
  snapshot: Snapshot | null;
  run: RunState;
  /** The provenance proof of the recorded real run. */
  proof: ProofState;
  /** What a Sepolia node says about the run's Ethereum payments, asked now. */
  settlement: SettlementState;
}

export interface ConsoleStore {
  getState(): ConsoleState;
  subscribe(listener: () => void): () => void;
  /** Reads the chain unless it has already been read; `force` reads again (the Refresh button). */
  load(force?: boolean): Promise<void>;
  dryRun(input: DryRunInput): Promise<void>;
  /** Computes the provenance proof unless it has been computed; `force` computes it again. */
  verify(force?: boolean): Promise<void>;
  /** Asks Sepolia for the receipts unless it has; `force` asks again. Never rejects. */
  confirm(force?: boolean): Promise<void>;
}

/**
 * Public NEAR testnet nodes that answer cross-origin browser requests, in order of preference. FastNEAR answers in
 * about 0.1 s; the NEAR-hosted node adds a flat 3 s to every call (measured), so it is the fallback.
 */
export const NEAR_RPC_ENDPOINTS = ['https://rpc.testnet.fastnear.com', 'https://rpc.testnet.near.org'] as const;

const IDLE: ConsoleState = { reading: 'idle', snapshot: null, run: { phase: 'idle' }, proof: { phase: 'idle' }, settlement: { phase: 'idle' } };

export function createConsoleStore(
  rpc: Rpc,
  now: () => number = Date.now,
  run: LiveRun = LIVE_RUN,
  evm: JsonRpc = createJsonRpc({ endpoints: SEPOLIA_RPC_ENDPOINTS, label: 'Sepolia RPC' }),
): ConsoleStore {
  let state: ConsoleState = IDLE;
  const listeners = new Set<() => void>();
  let loading: Promise<void> | null = null;
  let proving: Promise<void> | null = null;
  let confirming: Promise<void> | null = null;

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
    verify(force = false) {
      if (proving) return proving;
      if (state.proof.phase === 'done' && !force) return Promise.resolve();
      set({ proof: { phase: 'running', previous: state.proof.phase === 'done' ? state.proof.proof : null } });
      proving = proveProvenance(rpc, run)
        .then((proof) => set({ proof: { phase: 'done', proof } }))
        .finally(() => {
          proving = null;
        });
      return proving;
    },
    confirm(force = false) {
      if (confirming) return confirming;
      if (state.settlement.phase === 'done' && !force) return Promise.resolve();
      set({ settlement: { phase: 'running' } });
      confirming = confirmSettlement(evm, run)
        .then((byHash) => set({ settlement: { phase: 'done', byHash } }))
        .finally(() => {
          confirming = null;
        });
      return confirming;
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
