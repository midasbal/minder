// Real secp256k1 signatures, from an independent signer, over the signing
// hashes the tests produce. They let the tests exercise the contract's
// authenticity check (`ecrecover` against the configured sender) with
// signatures that are genuinely valid for a genuine hash, instead of
// well-formed numbers that recover to nothing in particular.
//
// Two fixed keys, both derived from public strings (they control no funds and
// are not secrets):
//   A: the configured sender. Its signatures must be accepted.
//   B: some other signer. Its signatures are well-formed but must be refused.
//
// Signatures are viem sign() (noble secp256k1, RFC 6979 nonces): deterministic
// and canonical (low s). Regenerate from a directory with viem installed at
// exactly the pinned version (the script refuses any other):
//
//   npm install viem@2.57.16
//   node /path/to/contract/vectors/gen-evm-signatures.mjs > /path/to/contract/vectors/evm-signatures.json
import { createRequire } from 'node:module';

const require = createRequire(process.cwd() + '/');
const { serializeTransaction, keccak256, toBytes, encodeFunctionData, erc20Abi } = require('viem');
const { sign, privateKeyToAccount } = require('viem/accounts');

const PINNED_VIEM = '2.57.16';
const viemVersion = require('viem/package.json').version;
if (viemVersion !== PINNED_VIEM) {
  console.error(`viem ${PINNED_VIEM} is required to reproduce the fixture, found ${viemVersion}`);
  process.exit(1);
}

const keys = {
  A: keccak256(toBytes('minder evm signing fixture key A v1')),
  B: keccak256(toBytes('minder evm signing fixture key B v1')),
};
const addresses = Object.fromEntries(Object.entries(keys).map(([name, key]) => [name, privateKeyToAccount(key).address.toLowerCase()]));

const TO = '0x70997970c51812dc3a010c7d01b50e0d17dc79c8';
// An address no test allowlists, for the owner's out-of-policy withdrawals.
const NOT_ALLOWLISTED = '0x00000000000000000000000000000000000000aa';

// Every transaction shape that reaches a signature in the tests.
const scenarios = [
  {
    // gate unit tests and the sandbox tests (a Sepolia-shaped configuration)
    name: 'unit-and-sandbox',
    base: { chainId: 11155111n, maxPriorityFeePerGas: 1_000_000_000n, maxFeePerGas: 2_000_000_000n, gas: 21000n },
    tos: [TO],
    nonces: [0n, 1n, 2n, 3n, 5n, 6n],
    values: [1000n],
  },
  {
    // owner_evm_withdraw in the gate tests and the sandbox: any recipient (an
    // address no policy allows, and the sender itself, as a cancellation
    // would send), any amount (zero, and far over every cap), explicit nonces
    name: 'owner-withdrawal',
    base: { chainId: 11155111n, maxPriorityFeePerGas: 1_000_000_000n, maxFeePerGas: 2_000_000_000n, gas: 21000n },
    tos: [NOT_ALLOWLISTED, 'SENDER'],
    nonces: [0n, 1n, 2n, 3n, 5n, 7n],
    values: [0n, 1000n, 5_000_000_000_000_000n],
  },
  {
    // the model-based harness: tiny fees so outlays stay small. Nonces up to
    // 63 cover the longest random sequence (40 actions) and the exhaustive depth.
    name: 'model-harness',
    base: { chainId: 5n, maxPriorityFeePerGas: 1n, maxFeePerGas: 5n, gas: 2n },
    tos: [TO],
    nonces: Array.from({ length: 64 }, (_, i) => BigInt(i)),
    values: [20n, 40n, 41n],
  },
];

// ERC-20 token transfers: what request_evm_token_payment builds. The calldata
// is viem's ABI encoding of transfer(recipient, amount), independent of the
// contract's hand-built bytes. The token is USDC on Sepolia.
const TOKEN = '0x1c7d4b196cb0c7b01d743fbc6116a902379c7238';
const EVM_ALLOWED = '0x70997970c51812dc3a010c7d01b50e0d17dc79c8';
const transferData = (to, amount) => encodeFunctionData({ abi: erc20Abi, functionName: 'transfer', args: [to, amount] });
const tokenScenarios = [
  {
    // the gate unit tests and the sandbox: the default token gas limit of 100000
    name: 'token-unit-and-sandbox',
    base: { chainId: 11155111n, maxPriorityFeePerGas: 1_000_000_000n, maxFeePerGas: 2_000_000_000n, gas: 100000n },
    recipients: [EVM_ALLOWED],
    amounts: [1000n, 2000n],
    nonces: [0n, 1n, 2n, 3n, 4n],
  },
  {
    // owner_evm_token_withdraw in the gate tests and the sandbox: any recipient (one no
    // policy allows, and the sender itself), any amount (zero and far over every cap),
    // explicit nonces, at the default token gas limit
    name: 'token-owner-withdrawal',
    base: { chainId: 11155111n, maxPriorityFeePerGas: 1_000_000_000n, maxFeePerGas: 2_000_000_000n, gas: 100000n },
    recipients: [EVM_ALLOWED, NOT_ALLOWLISTED, 'SENDER'],
    amounts: [0n, 1000n, 10n ** 30n],
    nonces: [0n, 1n, 2n, 3n, 5n, 7n],
  },
  {
    // the model harness: tiny fees, a token gas limit of 4, amounts at, below and over the cap
    name: 'token-model-harness',
    base: { chainId: 5n, maxPriorityFeePerGas: 1n, maxFeePerGas: 5n, gas: 4n },
    recipients: [EVM_ALLOWED],
    amounts: [30n, 50n, 51n],
    nonces: Array.from({ length: 64 }, (_, i) => BigInt(i)),
  },
];

const signatures = {};
for (const scenario of scenarios) {
  for (const recipient of scenario.tos) {
    const to = recipient === 'SENDER' ? addresses.A : recipient;
    for (const nonce of scenario.nonces) {
      for (const value of scenario.values) {
        const tx = { type: 'eip1559', ...scenario.base, to, nonce, value, data: '0x', accessList: [] };
        const hash = keccak256(serializeTransaction(tx));
        const entry = {};
        for (const [name, privateKey] of Object.entries(keys)) {
          const { r, s, yParity } = await sign({ hash, privateKey });
          // viem's own serialization of the signed transaction, so tests compare
          // the contract's emitted bytes against the reference, not against the code under test.
          entry[name] = { r, s, y_parity: yParity, raw_tx: serializeTransaction(tx, { r, s, yParity }) };
        }
        signatures[hash] = entry;
      }
    }
  }
}

for (const scenario of tokenScenarios) {
  for (const listed of scenario.recipients) {
    const recipient = listed === 'SENDER' ? addresses.A : listed;
    for (const nonce of scenario.nonces) {
      for (const amount of scenario.amounts) {
        const tx = { type: 'eip1559', ...scenario.base, to: TOKEN, nonce, value: 0n, data: transferData(recipient, amount), accessList: [] };
        const hash = keccak256(serializeTransaction(tx));
        const entry = {};
        for (const [name, privateKey] of Object.entries(keys)) {
          const { r, s, yParity } = await sign({ hash, privateKey });
          entry[name] = { r, s, y_parity: yParity, raw_tx: serializeTransaction(tx, { r, s, yParity }) };
        }
        signatures[hash] = entry;
      }
    }
  }
}

const header = {
  description:
    'Real signatures over the signing hashes the tests produce, by two fixed keys, with the signed serialization of each by viem. Keyed by the 0x signing hash. A is the configured sender; B is another signer.',
  viem: PINNED_VIEM,
  regenerate:
    'In a directory with `npm install viem@2.57.16`: node <repo>/contract/vectors/gen-evm-signatures.mjs > <repo>/contract/vectors/evm-signatures.json',
  keys:
    'A = keccak256("minder evm signing fixture key A v1"), B = keccak256("minder evm signing fixture key B v1"): derived from public strings, control no funds, not secrets',
  sender_a: addresses.A,
  sender_b: addresses.B,
  token_scenarios: tokenScenarios.map((s) => ({
    name: s.name,
    chain_id: s.base.chainId.toString(),
    token: TOKEN,
    gas_limit: s.base.gas.toString(),
    recipients: s.recipients,
    amounts: s.amounts.map(String),
    nonces: `${s.nonces[0]}..=${s.nonces[s.nonces.length - 1]}`,
  })),
  scenarios: scenarios.map((s) => ({
    name: s.name,
    chain_id: s.base.chainId.toString(),
    recipients: s.tos.length,
    nonces: `${s.nonces[0]}..=${s.nonces[s.nonces.length - 1]}`,
    values: s.values.map(String),
  })),
  hash_count: Object.keys(signatures).length,
};

process.stdout.write(
  '{\n"header": ' + JSON.stringify(header, null, 2) + ',\n"signatures": ' + JSON.stringify(signatures, null, 1) + '\n}\n',
);
