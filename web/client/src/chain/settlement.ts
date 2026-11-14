/*
  Whether the real run's Ethereum payments settled, asked of a Sepolia node now rather than read from the record.

  A reading never throws and never blocks anything: a node that cannot say leaves the recorded status standing,
  with the reason, and the page says which of the two it is showing.
*/

import type { LiveRun } from './liverun';
import { RpcFailure, type JsonRpc } from './rpc';

export type Settlement =
  | { state: 'success' | 'reverted'; blockNumber: number }
  /** The node has no receipt for the transaction (yet). */
  | { state: 'not-found' }
  | { state: 'unread'; reason: string };

/** Public Sepolia nodes that answer cross-origin browser requests, in order of preference. */
export const SEPOLIA_RPC_ENDPOINTS = ['https://ethereum-sepolia-rpc.publicnode.com', 'https://1rpc.io/sepolia'] as const;

async function receiptOf(evm: JsonRpc, hash: string): Promise<Settlement> {
  try {
    const { result } = await evm.call('eth_getTransactionReceipt', [hash]);
    if (result === null) return { state: 'not-found' };
    const receipt = result as { status?: string; blockNumber?: string; transactionHash?: string };
    if (receipt.transactionHash?.toLowerCase() !== hash.toLowerCase()) {
      return { state: 'unread', reason: 'the node answered for a different transaction' };
    }
    const blockNumber = Number.parseInt(receipt.blockNumber ?? '', 16);
    if (!Number.isSafeInteger(blockNumber)) return { state: 'unread', reason: 'the receipt has no block number' };
    if (receipt.status === '0x1') return { state: 'success', blockNumber };
    if (receipt.status === '0x0') return { state: 'reverted', blockNumber };
    return { state: 'unread', reason: 'the receipt has no recognisable status' };
  } catch (error) {
    return { state: 'unread', reason: error instanceof RpcFailure ? error.message : `unexpected answer (${(error as Error).message})` };
  }
}

/** The receipt of every Sepolia transaction in the run, by transaction hash. */
export async function confirmSettlement(evm: JsonRpc, run: LiveRun): Promise<Record<string, Settlement>> {
  const hashes = run.payments.flatMap((payment) => (payment.sepoliaTxHash === null ? [] : [payment.sepoliaTxHash]));
  const receipts = await Promise.all(hashes.map((hash) => receiptOf(evm, hash)));
  return Object.fromEntries(hashes.map((hash, index) => [hash, receipts[index]!]));
}
