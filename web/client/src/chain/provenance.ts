/*
  The provenance proof, computed in the browser.

  For a real Sepolia transaction the contract signed, three addresses must be one:
    1. the signer recovered from the signed transaction itself (the signature, not a label),
    2. the sender the contract is configured with (`get_evm_config`), and
    3. the address NEAR Chain Signatures derives for this contract account and path (the signer contract's
       `derived_public_key` view, turned into an address here).
  The signer contract derives its keys from the account that calls it, so an address derived for this contract
  can be signed for only by this contract. When all three match, that is the proof.

  The recovery is always local. The other two are read from the chain; if a read fails, the value recorded
  with the run stands in and is marked as recorded. Nothing is made up.
*/

import { CONTRACT_ACCOUNT, callView } from './contract';
import type { LiveRun } from './liverun';
import { RpcFailure, type Rpc } from './rpc';

const BASE58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';

export function base58Decode(text: string): Uint8Array {
  let value = 0n;
  for (const char of text) {
    const digit = BASE58.indexOf(char);
    if (digit < 0) throw new Error('not a base58 string');
    value = value * 58n + BigInt(digit);
  }
  const bytes: number[] = [];
  for (; value > 0n; value >>= 8n) bytes.unshift(Number(value & 255n));
  for (const char of text) {
    if (char !== '1') break;
    bytes.unshift(0);
  }
  return Uint8Array.from(bytes);
}

/** The EVM address of a NEAR-formatted secp256k1 public key: the last 20 bytes of keccak256 of its 64 bytes. */
export async function addressFromNearKey(key: string): Promise<string> {
  const match = /^secp256k1:([1-9A-HJ-NP-Za-km-z]+)$/.exec(key);
  if (!match) throw new Error('not a secp256k1 key');
  const bytes = base58Decode(match[1]!);
  if (bytes.length !== 64) throw new Error('a secp256k1 key is 64 bytes');
  const { keccak256, getAddress, bytesToHex } = await import('viem');
  return getAddress(`0x${keccak256(bytesToHex(bytes)).slice(-40)}`);
}

export interface Signed {
  address: string;
  to: string | null;
  nonce: number;
  chainId: number;
  value: bigint;
}

/** Parses a signed EVM transaction and recovers who signed it, from the signature alone. */
export async function recoverSigner(rawTx: string): Promise<Signed> {
  const { parseTransaction, recoverTransactionAddress } = await import('viem');
  const serialized = rawTx as `0x${string}`;
  const parsed = parseTransaction(serialized);
  const address = await recoverTransactionAddress({ serializedTransaction: serialized as never });
  return { address, to: parsed.to ?? null, nonce: parsed.nonce ?? 0, chainId: parsed.chainId ?? 0, value: parsed.value ?? 0n };
}

/** An address, and whether it was read from the chain now or is the value recorded with the run. */
export interface Sourced {
  address: string;
  source: 'live' | 'recorded';
  /** Why the live read was not used, when it was not. */
  reason: string | null;
}

export type Recovery = { ok: true; signed: Signed } | { ok: false; error: string };

export interface Proof {
  signer: string;
  path: string;
  sender: Sourced;
  derived: Sourced;
  /** One entry per EVM payment of the run, in order, keyed by its index in `run.payments`. */
  recoveries: Array<{ index: number; recovery: Recovery }>;
}

/** The address in EIP-55 checksum case, so the three shown are written alike. A string that is not an address is left as it is. */
async function checksum(address: string): Promise<string> {
  const { getAddress } = await import('viem');
  try {
    return getAddress(address);
  } catch {
    return address;
  }
}

const reasonOf = (error: unknown) => (error instanceof RpcFailure ? error.message : `unexpected answer (${(error as Error).message})`);

export async function proveProvenance(rpc: Rpc, run: LiveRun): Promise<Proof> {
  const recorded = run.recordedProvenance;
  let signer = recorded.signer;
  let path = recorded.derivationPath;
  let sender: Sourced = { address: await checksum(recorded.configuredSender), source: 'recorded', reason: null };
  try {
    const config = (await callView<{ sender: string; signer_id: string; derivation_path: string } | null>(rpc, 'get_evm_config')).value;
    if (config === null) throw new RpcFailure('rpc', 'the contract has no EVM configuration');
    signer = config.signer_id;
    path = config.derivation_path;
    sender = { address: await checksum(config.sender), source: 'live', reason: null };
  } catch (error) {
    sender = { ...sender, reason: reasonOf(error) };
  }

  let derived: Sourced = { address: await checksum(recorded.derivedAddress), source: 'recorded', reason: null };
  try {
    const key = (await callView<string>(rpc, 'derived_public_key', { path, predecessor: CONTRACT_ACCOUNT, domain_id: 0 }, signer)).value;
    derived = { address: await addressFromNearKey(key), source: 'live', reason: null };
  } catch (error) {
    derived = { ...derived, reason: reasonOf(error) };
  }

  const recoveries: Proof['recoveries'] = [];
  for (const [index, payment] of run.payments.entries()) {
    if (payment.rawTx === null) continue;
    try {
      recoveries.push({ index, recovery: { ok: true, signed: await recoverSigner(payment.rawTx) } });
    } catch (error) {
      recoveries.push({ index, recovery: { ok: false, error: `could not read the signed transaction (${(error as Error).message})` } });
    }
  }
  return { signer, path, sender, derived, recoveries };
}

const same = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();

/** True only when the recovered signer, the configured sender and the derived address are all one address. */
export function allThreeMatch(proof: Proof, recovery: Recovery): boolean {
  return recovery.ok && same(recovery.signed.address, proof.sender.address) && same(recovery.signed.address, proof.derived.address);
}
