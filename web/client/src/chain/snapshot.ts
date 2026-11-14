/*
  The live state of the contract, read from the chain and shaped for display.

  Every section is read on its own, so one failed call leaves a message in its place and the rest still
  show. Nothing here is a default or a stand-in: a value is either what the chain returned or an error.
*/

import { assetKey, describeAsset, formatAmount, type AssetId } from './assets';
import { CONTRACT_ACCOUNT, callView } from './contract';
import { RpcFailure, type Rpc } from './rpc';
import { WASM_SHA256 } from '../links';
import { formatUnits } from './units';

export type Section<T> = { ok: true; value: T } | { ok: false; message: string };

export interface KeyRow {
  publicKey: string;
  receiver: string;
  methods: string[];
  allowance: string;
}

export interface KeySummary {
  fullAccess: number;
  restricted: number;
  keys: KeyRow[];
}

export interface CodeSummary {
  bytes: number;
  sha256: string;
  expected: string;
  /** null when this page could not hash the code (no secure context). */
  matches: boolean | null;
}

export interface ContractFacts {
  status: string;
  owner: string;
  guardian: string | null;
  windowSeconds: number;
  buckets: number;
  timelockSeconds: number;
}

export interface MandateFacts {
  agentKey: string;
  expiry: string;
  epoch: number;
  revoked: boolean;
  assets: string[];
  nearRecipients: string[] | null;
  evmRecipients: string[] | null;
  perTxCap: string | null;
}

export interface AssetRow {
  key: string;
  asset: AssetId;
  label: string;
  /** The unit the figures below are in. */
  symbol: string;
  perTxCap: bigint;
  windowLimit: bigint;
  epochBudget: bigint | null;
  headroom: Section<{ windowRemaining: string; budgetRemaining: string | null }>;
  usage: Section<{ committed: string; reserved: string }>;
  perTx: string;
  window: string;
  budget: string | null;
}

export interface Snapshot {
  readAt: number;
  blockHeight: number;
  endpoint: string;
  contract: Section<ContractFacts>;
  mandate: Section<MandateFacts | null>;
  assets: Section<AssetRow[]>;
  nearRecipients: Section<string[]>;
  evmRecipients: Section<string[]>;
  keys: Section<KeySummary>;
  code: Section<CodeSummary>;
}

const NS_PER_SECOND = 1_000_000_000n;
const hex = (bytes: Uint8Array) => Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');

const messageOf = (error: unknown): string =>
  error instanceof RpcFailure ? error.message : `Unexpected answer from the chain (${(error as Error).message})`;

export async function sha256Hex(bytes: Uint8Array): Promise<string | null> {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) return null;
  return hex(new Uint8Array(await subtle.digest('SHA-256', bytes as BufferSource)));
}

interface Progress {
  blockHeight: number;
  endpoint: string;
}

export interface ReadOptions {
  now?: () => number;
  /** The wasm hash the deployed code is compared with: this repository's build. */
  expectedWasmSha256?: string;
}

export async function readSnapshot(rpc: Rpc, { now = Date.now, expectedWasmSha256 = WASM_SHA256 }: ReadOptions = {}): Promise<Snapshot> {
  const progress: Progress = { blockHeight: 0, endpoint: '' };
  const note = (blockHeight: number, endpoint: string) => {
    if (blockHeight >= progress.blockHeight) {
      progress.blockHeight = blockHeight;
      progress.endpoint = endpoint;
    }
  };
  const view = async <T,>(method: string, args: Record<string, unknown> = {}): Promise<T> => {
    const out = await callView<T>(rpc, method, args);
    note(out.blockHeight, out.trace.endpoint);
    return out.value;
  };
  const section = async <T,>(read: () => Promise<T>): Promise<Section<T>> => {
    try {
      return { ok: true, value: await read() };
    } catch (error) {
      return { ok: false, message: messageOf(error) };
    }
  };

  const contract = section<ContractFacts>(async () => {
    const [config, status] = await Promise.all([
      view<{ owner: string; window_len: string; num_buckets: number; timelock_delay: string; guardian?: string | null }>('get_config'),
      view<string>('get_status'),
    ]);
    return {
      status,
      owner: config.owner,
      guardian: config.guardian ?? null,
      windowSeconds: Number(BigInt(config.window_len) / NS_PER_SECOND),
      buckets: config.num_buckets,
      timelockSeconds: Number(BigInt(config.timelock_delay) / NS_PER_SECOND),
    };
  });

  const mandate = section<MandateFacts | null>(async () => {
    const raw = await view<{
      agent_key: string;
      expiry: string;
      epoch: number;
      revoked: boolean;
      scope: {
        allowed_assets: AssetId[] | null;
        allowed_near_recipients: string[] | null;
        allowed_evm_recipients: string[] | null;
        per_tx_cap: string | null;
      };
    } | null>('get_mandate');
    if (raw === null) return null;
    return {
      agentKey: raw.agent_key,
      expiry: new Date(Number(BigInt(raw.expiry) / 1_000_000n)).toISOString().slice(0, 10),
      epoch: raw.epoch,
      revoked: raw.revoked,
      assets: (raw.scope.allowed_assets ?? []).map((asset) => describeAsset(asset).label),
      nearRecipients: raw.scope.allowed_near_recipients,
      evmRecipients: raw.scope.allowed_evm_recipients,
      perTxCap: raw.scope.per_tx_cap,
    };
  });

  const assets = section<AssetRow[]>(async () => {
    const policies = await view<Array<[AssetId, { per_tx_cap: string; window_limit: string; epoch_budget: string | null }]>>(
      'get_asset_policies',
    );
    return Promise.all(
      policies.map(async ([asset, policy]) => {
        const perTxCap = BigInt(policy.per_tx_cap);
        const windowLimit = BigInt(policy.window_limit);
        const epochBudget = policy.epoch_budget === null ? null : BigInt(policy.epoch_budget);
        const [headroom, usage] = await Promise.all([
          section(async () => {
            const room = await view<{ per_tx_cap: string; window_remaining: string; budget_remaining: string | null } | null>(
              'get_headroom',
              { asset },
            );
            if (room === null) throw new RpcFailure('rpc', 'the contract has no headroom for this asset');
            return {
              windowRemaining: formatAmount(asset, BigInt(room.window_remaining)),
              budgetRemaining: room.budget_remaining === null ? null : formatAmount(asset, BigInt(room.budget_remaining)),
            };
          }),
          section(async () => {
            const spent = await view<{ committed: string; reserved: string }>('get_budget_usage', { asset });
            return { committed: formatAmount(asset, BigInt(spent.committed)), reserved: formatAmount(asset, BigInt(spent.reserved)) };
          }),
        ]);
        return {
          key: assetKey(asset),
          asset,
          label: describeAsset(asset).label,
          symbol: describeAsset(asset).symbol,
          perTxCap,
          windowLimit,
          epochBudget,
          headroom,
          usage,
          perTx: formatAmount(asset, perTxCap),
          window: formatAmount(asset, windowLimit),
          budget: epochBudget === null ? null : formatAmount(asset, epochBudget),
        };
      }),
    );
  });

  const nearRecipients = section(() => view<string[]>('get_recipients'));
  const evmRecipients = section(() => view<string[]>('get_evm_recipients'));

  const keys = section<KeySummary>(async () => {
    const { result, trace } = await rpc.query({ request_type: 'view_access_key_list', finality: 'final', account_id: CONTRACT_ACCOUNT });
    note((result as { block_height?: number }).block_height ?? 0, trace.endpoint);
    const list = (result as { keys?: Array<{ public_key: string; access_key: { permission: unknown } }> }).keys;
    if (!Array.isArray(list)) throw new RpcFailure('rpc', 'the answer had no key list');
    let fullAccess = 0;
    const rows: KeyRow[] = [];
    for (const { public_key, access_key } of list) {
      const permission = access_key.permission as 'FullAccess' | { FunctionCall: { allowance: string | null; receiver_id: string; method_names: string[] } };
      if (permission === 'FullAccess') {
        fullAccess += 1;
      } else {
        const call = permission.FunctionCall;
        rows.push({
          publicKey: public_key,
          receiver: call.receiver_id,
          methods: call.method_names,
          allowance: call.allowance === null ? 'unlimited' : `${formatUnits(BigInt(call.allowance), 24)} NEAR`,
        });
      }
    }
    return { fullAccess, restricted: rows.length, keys: rows };
  });

  const code = section<CodeSummary>(async () => {
    const { result, trace } = await rpc.query({ request_type: 'view_code', finality: 'final', account_id: CONTRACT_ACCOUNT });
    note((result as { block_height?: number }).block_height ?? 0, trace.endpoint);
    const encoded = (result as { code_base64?: string }).code_base64;
    if (typeof encoded !== 'string') throw new RpcFailure('rpc', 'the answer had no code');
    const binary = atob(encoded);
    const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));
    const digest = await sha256Hex(bytes);
    if (digest === null) throw new RpcFailure('rpc', 'this browser cannot hash the code here (it needs a secure page, https)');
    return { bytes: bytes.length, sha256: digest, expected: expectedWasmSha256, matches: digest === expectedWasmSha256 };
  });

  const [contractSection, mandateSection, assetsSection, nearSection, evmSection, keysSection, codeSection] = await Promise.all([
    contract,
    mandate,
    assets,
    nearRecipients,
    evmRecipients,
    keys,
    code,
  ]);

  return {
    readAt: now(),
    blockHeight: progress.blockHeight,
    endpoint: progress.endpoint,
    contract: contractSection,
    mandate: mandateSection,
    assets: assetsSection,
    nearRecipients: nearSection,
    evmRecipients: evmSection,
    keys: keysSection,
    code: codeSection,
  };
}

/** True when the page reached the chain for at least one section. */
export function reachedChain(snapshot: Snapshot): boolean {
  return [
    snapshot.contract,
    snapshot.mandate,
    snapshot.assets,
    snapshot.nearRecipients,
    snapshot.evmRecipients,
    snapshot.keys,
    snapshot.code,
  ].some((section) => section.ok);
}
