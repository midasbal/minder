/** The assets and request methods of the live deployment, and how to show them. */

import { formatUnits } from './units';

/** The agent's PUBLIC key. It is public by design: it names the mandate a dry run is evaluated for. */
export const AGENT_PUBLIC_KEY = 'ed25519:GysmbJj8RtV39PxNepF2ysGx3byLGvJvsofqe8JK5gfF';

/** Circle's USDC on Sepolia: the contract names tokens by address only, so the symbol is ours. */
export const USDC_ADDRESS = '0x1c7d4b196cb0c7b01d743fbc6116a902379c7238';

export type AssetId = 'Near' | { Evm: number } | { Evm20: { chain_id: number; token: number[] } };

export interface AssetInfo {
  label: string;
  symbol: string;
  decimals: number;
}

export function tokenAddress(bytes: readonly number[]): string {
  return `0x${bytes.map((byte) => byte.toString(16).padStart(2, '0')).join('')}`;
}

export function describeAsset(asset: AssetId): AssetInfo {
  if (asset === 'Near') return { label: 'NEAR', symbol: 'NEAR', decimals: 24 };
  if ('Evm' in asset) return { label: 'ETH on Sepolia', symbol: 'ETH', decimals: 18 };
  const address = tokenAddress(asset.Evm20.token);
  if (address === USDC_ADDRESS) return { label: 'USDC on Sepolia', symbol: 'USDC', decimals: 6 };
  return { label: `ERC-20 ${address}`, symbol: 'tokens', decimals: 0 };
}

export function formatAsset(asset: AssetId, value: bigint): string {
  const { symbol, decimals } = describeAsset(asset);
  return `${formatUnits(value, decimals)} ${symbol}`;
}

/** The amount without its unit, for a table whose column or row already names the asset. */
export function formatAmount(asset: AssetId, value: bigint): string {
  return formatUnits(value, describeAsset(asset).decimals);
}

export function assetKey(asset: AssetId): string {
  return JSON.stringify(asset);
}

export type MethodId = 'request_local_transfer' | 'request_evm_payment' | 'request_evm_token_payment';

export interface MethodSpec {
  id: MethodId;
  /** The short name on the selector. */
  name: string;
  symbol: string;
  decimals: number;
  recipient: 'near' | 'evm';
  /** What the contract receives, as the page converts what the visitor types. */
  baseUnit: string;
  /** The view that answers it, and the placeholder for the recipient. */
  view: 'dry_run' | 'dry_run_evm' | 'dry_run_evm_token';
  placeholder: string;
}

export const METHODS: readonly MethodSpec[] = [
  {
    id: 'request_local_transfer',
    name: 'NEAR',
    symbol: 'NEAR',
    decimals: 24,
    recipient: 'near',
    baseUnit: 'yoctoNEAR',
    view: 'dry_run',
    placeholder: 'account.testnet',
  },
  {
    id: 'request_evm_payment',
    name: 'ETH on Sepolia',
    symbol: 'ETH',
    decimals: 18,
    recipient: 'evm',
    baseUnit: 'wei',
    view: 'dry_run_evm',
    placeholder: '0x…',
  },
  {
    id: 'request_evm_token_payment',
    name: 'USDC on Sepolia',
    symbol: 'USDC',
    decimals: 6,
    recipient: 'evm',
    baseUnit: 'base units, 1 USDC = 1,000,000',
    view: 'dry_run_evm_token',
    placeholder: '0x…',
  },
];

/** What the amount box means: the visitor types whole units; the contract is sent base units. */
export function amountHint(spec: MethodSpec): string {
  return `You enter ${spec.symbol}; the contract receives ${spec.baseUnit} (${spec.decimals} decimals).`;
}

export function methodSpec(id: MethodId): MethodSpec {
  const spec = METHODS.find((method) => method.id === id);
  if (!spec) throw new Error(`unknown method ${id}`);
  return spec;
}

/** Whether a policy asset is the one a method pays out of. */
export function assetOfMethod(method: MethodId, asset: AssetId): boolean {
  if (method === 'request_local_transfer') return asset === 'Near';
  if (method === 'request_evm_payment') return typeof asset === 'object' && 'Evm' in asset;
  return typeof asset === 'object' && 'Evm20' in asset;
}

const NEAR_ACCOUNT = /^(([a-z\d]+[-_])*[a-z\d]+\.)*([a-z\d]+[-_])*[a-z\d]+$/;
const EVM_ADDRESS = /^0x[0-9a-fA-F]{40}$/;

/** A format check on what the visitor typed; the allowlist and scope are the contract's to judge. */
export function recipientError(kind: MethodSpec['recipient'], text: string): string | null {
  const value = text.trim();
  if (kind === 'near') {
    return value.length >= 2 && value.length <= 64 && NEAR_ACCOUNT.test(value)
      ? null
      : 'Enter a NEAR account, such as vendor.example.testnet';
  }
  return EVM_ADDRESS.test(value) ? null : 'Enter a 0x address: 0x and 40 hex digits';
}
