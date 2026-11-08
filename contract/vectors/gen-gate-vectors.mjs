// Reference values for tests/evm_gate.rs, produced by viem: for the fixed
// gate configuration used in the sandbox tests, the signing hash the contract
// must send to the signer and the raw signed transaction it must emit for the
// mock signer's fixed signature, at nonces 0 and 1.
//
//   npm install viem@2.57.16 && node /path/to/gen-gate-vectors.mjs
import { createRequire } from 'node:module';

const { serializeTransaction, keccak256 } = createRequire(process.cwd() + '/')('viem');

const config = {
  type: 'eip1559',
  chainId: 11155111,
  maxPriorityFeePerGas: 1_000_000_000n,
  maxFeePerGas: 2_000_000_000n,
  gas: 21000n,
  to: '0x70997970C51812dc3A010C7d01b50e0d17dc79C8',
  value: 1000n,
  data: '0x',
  accessList: [],
};
// The mock signer's response: affine_point = "02" + r, scalar = s, recovery_id = 1.
const sig = {
  r: '0x5b2fc0e1f6d4a7c98e3b1d2a4f5c6e7d8a9b0c1d2e3f405162738495a6b7c8d9',
  s: '0x2a4d6f8091b3d5f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f',
  yParity: 1,
};

for (const nonce of [0, 1]) {
  const tx = { ...config, nonce };
  const unsigned = serializeTransaction(tx);
  console.log(JSON.stringify({
    nonce,
    signing_hash: keccak256(unsigned),
    raw_tx: serializeTransaction(tx, sig),
  }, null, 2));
}
