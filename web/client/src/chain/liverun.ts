/*
  The recorded real run: the payments the live contract gated, signed and settled, as saved by the operator in
  data/live-run.json. It is read once, here, and checked for shape; every link is built from a transaction hash
  rather than taken from the file, so a link can never point somewhere the hash does not.
*/

import recorded from '../data/live-run.json';

export type PaymentKind = 'near_transfer' | 'evm_eth' | 'evm_erc20';

export interface RecordedPayment {
  kind: PaymentKind;
  method: string;
  amountDisplay: string;
  recipient: string;
  nearTxHash: string;
  /** The signed transaction the contract emitted, for the EVM payments. */
  rawTx: string | null;
  sepoliaTxHash: string | null;
  status: string;
  sepoliaBlock: string | null;
  timestamp: string;
}

export interface LiveRun {
  contract: string;
  vendorEvmAddress: string;
  vendorNearAccount: string;
  recordedAt: string;
  payments: RecordedPayment[];
  /** What the run's own provenance check found, kept to fall back on when a live read fails. */
  recordedProvenance: { signer: string; derivationPath: string; configuredSender: string; derivedAddress: string };
}

export const nearTxUrl = (hash: string) => `https://testnet.nearblocks.io/txns/${hash}`;
export const sepoliaTxUrl = (hash: string) => `https://sepolia.etherscan.io/tx/${hash}`;

const KINDS: readonly PaymentKind[] = ['near_transfer', 'evm_eth', 'evm_erc20'];

type Record_ = Record<string, unknown>;

function text(from: Record_, key: string, where: string): string {
  const value = from[key];
  if (typeof value !== 'string' || value === '') throw new Error(`live-run.json: ${where}.${key} is missing`);
  return value;
}

function optionalText(from: Record_, key: string): string | null {
  const value = from[key];
  return typeof value === 'string' && value !== '' ? value : null;
}

/** Reads the recorded run, or throws naming what is wrong with it. */
export function parseLiveRun(raw: unknown): LiveRun {
  const root = raw as Record_;
  const payments = root.payments;
  if (!Array.isArray(payments) || payments.length === 0) throw new Error('live-run.json: no payments');
  const provenance = root.provenance as Record_;
  return {
    contract: text(root, 'contract', 'run'),
    vendorEvmAddress: text(root, 'vendorEvmAddress', 'run'),
    vendorNearAccount: text(root, 'vendorNearAccount', 'run'),
    recordedAt: text(root, 'recordedAt', 'run'),
    payments: payments.map((entry, index): RecordedPayment => {
      const payment = entry as Record_;
      const where = `payments[${index}]`;
      const kind = text(payment, 'kind', where) as PaymentKind;
      if (!KINDS.includes(kind)) throw new Error(`live-run.json: ${where}.kind ${kind} is unknown`);
      const evm = kind !== 'near_transfer';
      const rawTx = optionalText(payment, 'rawTx');
      const sepoliaTxHash = optionalText(payment, 'sepoliaTxHash');
      if (evm && (rawTx === null || sepoliaTxHash === null)) throw new Error(`live-run.json: ${where} is an EVM payment without its raw transaction`);
      return {
        kind,
        method: text(payment, 'method', where),
        amountDisplay: text(payment, 'amountDisplay', where),
        recipient: text(payment, 'recipient', where),
        nearTxHash: text(payment, 'nearTxHash', where),
        rawTx,
        sepoliaTxHash,
        status: text(payment, 'status', where),
        sepoliaBlock: optionalText(payment, 'sepoliaBlock'),
        timestamp: text(payment, 'timestamp', where),
      };
    }),
    recordedProvenance: {
      signer: text(provenance, 'signer', 'provenance'),
      derivationPath: text(provenance, 'derivationPath', 'provenance'),
      configuredSender: text(provenance, 'configuredSender', 'provenance'),
      derivedAddress: text(provenance, 'derivedAddressFromSignerView', 'provenance'),
    },
  };
}

export const LIVE_RUN: LiveRun = parseLiveRun(recorded);
