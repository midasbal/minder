/** Calls to the live contract's view methods, and the dry run that is the visitor's attack surface. */

import { AGENT_PUBLIC_KEY, methodSpec, type MethodId } from './assets';
import { RpcFailure, type Rpc, type RpcTrace } from './rpc';

export const CONTRACT_ACCOUNT = 'gate.minder-gate-owner-bf5fcd.testnet';

export interface ViewOutcome<T> {
  value: T;
  blockHeight: number;
  trace: RpcTrace & { args: unknown; method: string };
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

function toBase64(text: string): string {
  let binary = '';
  for (const byte of encoder.encode(text)) binary += String.fromCharCode(byte);
  return btoa(binary);
}

/** A free view call at the latest final block (on the contract unless another account is named). No key, no gas, no state change. */
export async function callView<T>(
  rpc: Rpc,
  method: string,
  args: Record<string, unknown> = {},
  account: string = CONTRACT_ACCOUNT,
): Promise<ViewOutcome<T>> {
  const { result, trace } = await rpc.query({
    request_type: 'call_function',
    finality: 'final',
    account_id: account,
    method_name: method,
    args_base64: toBase64(JSON.stringify(args)),
  });
  const body = result as { result?: number[]; error?: string; block_height?: number } | null;
  if (body === null || typeof body !== 'object') throw new RpcFailure('rpc', 'the answer had no result');
  if (typeof body.error === 'string') throw new RpcFailure('contract', body.error);
  if (!Array.isArray(body.result)) throw new RpcFailure('rpc', 'the answer had no result bytes');
  let value: T;
  try {
    value = JSON.parse(decoder.decode(Uint8Array.from(body.result))) as T;
  } catch {
    throw new RpcFailure('rpc', `${method} did not return JSON`);
  }
  return { value, blockHeight: body.block_height ?? 0, trace: { ...trace, args, method } };
}

/** One check of a request, as the contract's own enum: allowed, or denied with a code. */
export type Verdict = { allow: true } | { allow: false; reason: string };

export function parseDecision(raw: unknown): Verdict {
  if (raw === 'Allow') return { allow: true };
  if (typeof raw === 'object' && raw !== null && 'Deny' in raw && typeof (raw as { Deny: unknown }).Deny === 'string') {
    return { allow: false, reason: (raw as { Deny: string }).Deny };
  }
  throw new RpcFailure('rpc', `unrecognised decision ${JSON.stringify(raw)}`);
}

export interface DryRunInput {
  method: MethodId;
  recipient: string;
  /** In the asset's base units: yocto, wei, or USDC's six decimals. */
  amount: bigint;
  /** The ERC-20 token, for a token payment only: taken from the live policy, never typed. */
  token?: number[];
}

/** One leg of a request. A token payment has two: the token, and the ETH gas it costs to send. */
export interface Leg {
  name: 'payment' | 'token' | 'gas';
  verdict: Verdict;
}

export interface DryRunResult {
  allowed: boolean;
  legs: Leg[];
  trace: ViewOutcome<unknown>['trace'];
}

/** Asks the live contract what the agent would get for this request right now. Sends nothing. */
export async function dryRun(rpc: Rpc, input: DryRunInput): Promise<DryRunResult> {
  const spec = methodSpec(input.method);
  const recipient = input.recipient.trim();
  const amount = input.amount.toString();
  const base = { agent_key: AGENT_PUBLIC_KEY, recipient, amount };
  let legs: Leg[];
  let trace: DryRunResult['trace'];

  if (spec.view === 'dry_run') {
    const out = await callView<unknown>(rpc, 'dry_run', { ...base, asset: 'Near' });
    legs = [{ name: 'payment', verdict: parseDecision(out.value) }];
    trace = out.trace;
  } else if (spec.view === 'dry_run_evm') {
    const out = await callView<unknown>(rpc, 'dry_run_evm', base);
    legs = [{ name: 'payment', verdict: parseDecision(out.value) }];
    trace = out.trace;
  } else {
    if (!input.token) throw new RpcFailure('rpc', 'the live policy names no token to pay');
    const out = await callView<{ token: unknown; gas: unknown }>(rpc, 'dry_run_evm_token', {
      ...base,
      token: `0x${input.token.map((byte) => byte.toString(16).padStart(2, '0')).join('')}`,
    });
    legs = [
      { name: 'token', verdict: parseDecision(out.value.token) },
      { name: 'gas', verdict: parseDecision(out.value.gas) },
    ];
    trace = out.trace;
  }
  return { allowed: legs.every((leg) => leg.verdict.allow), legs, trace };
}
