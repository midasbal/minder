/*
  The red-team log and the freeze drill: what a compromised agent tried against the live contract, and how the
  contract was stopped and restarted, as recorded from real transactions on testnet (data/red-team.json and
  data/freeze-drill.json). Read once, here, and checked for shape; a malformed file fails the build rather than
  showing part of the story. Every explorer link is built from a transaction hash.
*/

import drillFile from '../data/freeze-drill.json';
import redTeamFile from '../data/red-team.json';
import { nearTxUrl } from './liverun';

type Obj = Record<string, unknown>;

function text(from: Obj, key: string, where: string): string {
  const value = from[key];
  if (typeof value !== 'string' || value === '') throw new Error(`${where}.${key} is missing`);
  return value;
}

function list(from: Obj, key: string, where: string): Obj[] {
  const value = from[key];
  if (!Array.isArray(value) || value.length === 0) throw new Error(`${where}.${key} is missing`);
  return value as Obj[];
}

/** A transaction that reached a block. */
export interface OnChainAttempt {
  id: string;
  attack: string;
  method: string;
  /** What the contract answered: a deny code, or null when it allowed the request. */
  reason: string | null;
  /** The leg that decided, for a token payment. */
  leg: string | null;
  allowed: boolean;
  tx: string;
  blockTime: string;
}

/** A call the NEAR runtime refused before any contract code ran. It never reached a block. */
export interface ProtocolRefusal {
  id: string;
  attack: string;
  error: string;
  /** The hash the transaction would have had, never included in a block. */
  signedTransactionHash: string;
  rawRpcResponse: unknown;
}

export interface RedTeam {
  contract: string;
  startedAt: string;
  denials: OnChainAttempt[];
  windowExplanation: string;
  window: OnChainAttempt[];
  refusals: ProtocolRefusal[];
}

function attempt(entry: Obj, where: string): OnChainAttempt {
  const status = text(entry, 'finalStatus', where);
  const event = entry.decisionEvent as Obj | null;
  const allowed = status === 'SuccessValue';
  const reason = typeof entry.reason === 'string' ? entry.reason : null;
  if (!allowed && reason === null) throw new Error(`${where}: a refused attempt names no reason`);
  if (!allowed && (!event || event.decision !== 'deny' || event.reason !== reason)) {
    throw new Error(`${where}: the recorded decision event does not match the reason`);
  }
  return {
    id: text(entry, 'id', where),
    attack: text(entry, 'attack', where),
    method: text(entry, 'method', where),
    reason,
    leg: typeof entry.leg === 'string' ? entry.leg : null,
    allowed,
    tx: text(entry, 'tx', where),
    blockTime: text(entry, 'blockTime', where),
  };
}

export function parseRedTeam(raw: unknown): RedTeam {
  const root = raw as Obj;
  const window = root.window as Obj;
  return {
    contract: text(root, 'contract', 'red-team'),
    startedAt: text(root, 'startedAt', 'red-team'),
    denials: list(root, 'onChainDenials', 'red-team').map((entry, i) => attempt(entry, `onChainDenials[${i}]`)),
    windowExplanation: text(window, 'explanation', 'window'),
    window: list(window, 'entries', 'window').map((entry, i) => attempt(entry, `window[${i}]`)),
    refusals: list(root, 'protocolRefusals', 'red-team').map((entry, i): ProtocolRefusal => {
      const where = `protocolRefusals[${i}]`;
      const error = text(entry, 'expectedError', where);
      if (!JSON.stringify(entry.rawRpcResponse).includes(error)) throw new Error(`${where}: the raw RPC error does not contain ${error}`);
      return {
        id: text(entry, 'id', where),
        attack: text(entry, 'attack', where),
        error,
        signedTransactionHash: text(entry, 'signedTransactionHash', where),
        rawRpcResponse: entry.rawRpcResponse,
      };
    }),
  };
}

export type DrillStep = 'freeze' | 'denied-while-frozen' | 'propose-unfreeze' | 'execute-unfreeze';

export interface DrillEntry {
  id: DrillStep;
  description: string;
  signer: string;
  tx: string;
  blockTime: string;
  /** The deny code, for the agent's refused request. */
  reason: string | null;
}

export interface FreezeDrill {
  delaySeconds: number;
  finalStatus: string;
  entries: DrillEntry[];
  /** When the queued unfreeze became executable, on chain time. */
  readyAt: string;
}

const STEPS: readonly DrillStep[] = ['freeze', 'denied-while-frozen', 'propose-unfreeze', 'execute-unfreeze'];

export function parseFreezeDrill(raw: unknown): FreezeDrill {
  const root = raw as Obj;
  const entries = list(root, 'entries', 'freeze-drill').map((entry, i): DrillEntry => {
    const where = `entries[${i}]`;
    return {
      id: text(entry, 'id', where) as DrillStep,
      description: text(entry, 'description', where),
      signer: text(entry, 'signer', where),
      tx: text(entry, 'tx', where),
      blockTime: text(entry, 'blockTime', where),
      reason: typeof entry.reason === 'string' ? entry.reason : null,
    };
  });
  if (entries.map((entry) => entry.id).join() !== STEPS.join()) throw new Error('freeze-drill: the steps are not freeze, refusal, propose, execute');
  const propose = (root.entries as Obj[])[2]!;
  const pending = propose.pendingChange as Obj;
  const nanos = (key: string) => Number(BigInt(text(pending, key, 'pendingChange')) / 1_000_000n);
  const delaySeconds = (nanos('ready_at') - nanos('proposed_at')) / 1000;
  return {
    delaySeconds,
    finalStatus: text(root, 'finalStatus', 'freeze-drill'),
    entries,
    readyAt: new Date(nanos('ready_at')).toISOString(),
  };
}

export const RED_TEAM: RedTeam = parseRedTeam(redTeamFile);
export const FREEZE_DRILL: FreezeDrill = parseFreezeDrill(drillFile);
export { nearTxUrl };
