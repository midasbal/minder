// Reference vectors for src/evm.rs, produced by viem (an independent
// implementation). Run from a directory where viem is installed:
//
//   npm install viem@2.57.16 && node /path/to/gen-evm-vectors.mjs
//
// Prints one JSON object per case. The Rust tests hard-code these outputs.
// The signatures are arbitrary fixed values: serialization does not need a
// valid signature, and some deliberately carry leading zero bytes to exercise
// integer trimming of r and s.
import { createRequire } from 'node:module';

const { serializeTransaction, keccak256 } = createRequire(process.cwd() + '/')('viem');

const TO = '0x70997970C51812dc3A010C7d01b50e0d17dc79C8';
const U64_MAX = 2n ** 64n - 1n;
const U128_MAX = 2n ** 128n - 1n;

const cases = [
  {
    name: 'plain_transfer',
    tx: { chainId: 11155111n, nonce: 7n, maxPriorityFeePerGas: 1_500_000_000n, maxFeePerGas: 30_000_000_000n, gas: 21000n, to: TO, value: 100_000_000_000_000n },
    sig: { r: '0x5b2fc0e1f6d4a7c98e3b1d2a4f5c6e7d8a9b0c1d2e3f405162738495a6b7c8d9', s: '0x2a4d6f8091b3d5f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f', yParity: 1 },
  },
  {
    name: 'zero_fields',
    tx: { chainId: 1n, nonce: 0n, maxPriorityFeePerGas: 0n, maxFeePerGas: 7n, gas: 21000n, to: TO, value: 0n },
    sig: { r: '0x0000a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e', s: '0x00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff', yParity: 0 },
  },
  {
    name: 'single_byte_boundaries',
    tx: { chainId: 127n, nonce: 127n, maxPriorityFeePerGas: 128n, maxFeePerGas: 255n, gas: 256n, to: TO, value: 1n },
    sig: { r: '0x0000000000000000000000000000000000000000000000000000000000000080', s: '0x0000000000000000000000000000000000000000000000000000000000000001', yParity: 1 },
  },
  // Payloads of exactly 55 and 56 bytes: the largest short-form and smallest
  // long-form RLP list headers (0xf7 and 0xf8 0x38).
  {
    name: 'list_payload_55_bytes',
    tx: { chainId: 1n, nonce: 0n, maxPriorityFeePerGas: 0x0102030405n, maxFeePerGas: 0x010203040506n, gas: 21000n, to: TO, value: 0x0102030405060708090a0b0c0dn },
    sig: { r: '0x00000000000000000000000000000000000000000000000000000000000000ff', s: '0x0000000000000000000000000000000000000000000000000000000000000100', yParity: 0 },
  },
  {
    name: 'list_payload_56_bytes',
    tx: { chainId: 1n, nonce: 0n, maxPriorityFeePerGas: 0x0102030405n, maxFeePerGas: 0x010203040506n, gas: 21000n, to: TO, value: 0x0102030405060708090a0b0c0d0en },
    sig: { r: '0x00000000000000000000000000000000000000000000000000000000000000ff', s: '0x0000000000000000000000000000000000000000000000000000000000000100', yParity: 1 },
  },
  {
    name: 'high_top_bytes',
    tx: { chainId: U64_MAX, nonce: U64_MAX, maxPriorityFeePerGas: U128_MAX, maxFeePerGas: U128_MAX, gas: U64_MAX, to: '0xffffffffffffffffffffffffffffffffffffffff', value: U128_MAX },
    sig: { r: '0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff', s: '0x7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0', yParity: 1 },
  },
];

for (const { name, tx, sig } of cases) {
  const base = { type: 'eip1559', ...tx, data: '0x', accessList: [] };
  const unsigned = serializeTransaction(base);
  const signed = serializeTransaction(base, sig);
  console.log(JSON.stringify({
    name,
    fields: Object.fromEntries(Object.entries(tx).map(([k, v]) => [k, typeof v === 'bigint' ? v.toString() : v])),
    sig,
    unsigned,
    signing_hash: keccak256(unsigned),
    signed,
  }, null, 2));
}
