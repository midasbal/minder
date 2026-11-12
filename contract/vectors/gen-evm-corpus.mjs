// Differential corpus for src/evm.rs: a large, seeded set of EIP-1559 value
// transfers with viem's serialization of each, checked byte for byte by
// tests/evm_corpus.rs. Deterministic: a fixed PRNG seed and no clock, so the
// output is identical on every run and every machine.
//
// Regenerate from a directory where viem is installed at exactly the pinned
// version (the script refuses to run against any other):
//
//   npm install viem@2.57.16
//   node /path/to/contract/vectors/gen-evm-corpus.mjs > /path/to/contract/vectors/evm-corpus.json
//
// Shape under test: type-2 transaction with an empty access list. Most cases
// are value transfers with empty data; the last sections add calldata: ERC-20
// transfer calls as request_evm_token_payment builds them (the calldata is
// viem's ABI encoding, independent of the contract's hand-built bytes) and data
// at the RLP length edges.
import { createRequire } from 'node:module';

const require = createRequire(process.cwd() + '/');
const { serializeTransaction, keccak256, toBytes, encodeFunctionData, erc20Abi } = require('viem');
const { sign } = require('viem/accounts');

const PINNED_VIEM = '2.57.16';
const viemVersion = require('viem/package.json').version;
if (viemVersion !== PINNED_VIEM) {
  console.error(`viem ${PINNED_VIEM} is required to reproduce the corpus, found ${viemVersion}`);
  process.exit(1);
}

const SEED = 0x4d494e4445520001n; // "MINDER" + 1
const U64_MAX = 2n ** 64n - 1n;
const U128_MAX = 2n ** 128n - 1n;
// secp256k1 group order and half of it, rounded down.
const N = 0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141n;
const HALF = N / 2n;

// ---- PRNG: SplitMix64 over BigInt, platform independent ----

class Rng {
  constructor(seed) {
    this.state = BigInt.asUintN(64, seed);
  }
  next() {
    this.state = BigInt.asUintN(64, this.state + 0x9e3779b97f4a7c15n);
    let z = this.state;
    z = BigInt.asUintN(64, (z ^ (z >> 30n)) * 0xbf58476d1ce4e5b9n);
    z = BigInt.asUintN(64, (z ^ (z >> 27n)) * 0x94d049bb133111ebn);
    return z ^ (z >> 31n);
  }
  /** Uniform integer in [lo, hi]. */
  int(lo, hi) {
    return lo + Number(this.next() % BigInt(hi - lo + 1));
  }
  /** A value with a uniformly chosen bit length in [0, maxBits]: every RLP width occurs. */
  bits(maxBits) {
    const length = this.int(0, maxBits);
    if (length === 0) return 0n;
    let value = 0n;
    for (let got = 0; got < length; got += 64) value = (value << 64n) | this.next();
    return BigInt.asUintN(length, value) | (1n << BigInt(length - 1));
  }
  bytes(count) {
    let out = '';
    for (let i = 0; i < count; i++) out += Number(this.next() & 0xffn).toString(16).padStart(2, '0');
    return out;
  }
}

// ---- Sizes, to aim at the RLP list-length boundary ----

const bitLength = (v) => (v === 0n ? 0 : v.toString(2).length);
const byteLength = (v) => Math.ceil(bitLength(v) / 8);
/** Bytes an integer takes as an RLP item. */
const uintSize = (v) => (v === 0n ? 1 : v < 128n ? 1 : 1 + byteLength(v));
/** `to` (21) + empty data (1) + empty access list (1). */
const FIXED_PAYLOAD = 23;
const unsignedPayload = (t) =>
  FIXED_PAYLOAD + [t.chainId, t.nonce, t.maxPriorityFeePerGas, t.maxFeePerGas, t.gas, t.value].reduce((n, v) => n + uintSize(v), 0);

// ---- Transactions ----

const rng = new Rng(SEED);

const hex = (v) => '0x' + v.toString(16).padStart(64, '0');
const ZERO_ADDRESS = '0x' + '00'.repeat(20);
const ONES_ADDRESS = '0x' + 'ff'.repeat(20);

/** A valid (tip <= fee, chain id >= 1) transaction with every field random in its full range. */
function randomTx() {
  const maxFeePerGas = rng.bits(128);
  let maxPriorityFeePerGas = rng.bits(bitLength(maxFeePerGas));
  if (maxPriorityFeePerGas > maxFeePerGas) maxPriorityFeePerGas = maxFeePerGas;
  return {
    chainId: rng.bits(64) || 1n,
    nonce: rng.bits(64),
    maxPriorityFeePerGas,
    maxFeePerGas,
    gas: rng.bits(64),
    to: '0x' + rng.bytes(20),
    value: rng.bits(128),
  };
}

const FIELDS = {
  chainId: { max: U64_MAX, min: 1n, bits: 64 },
  nonce: { max: U64_MAX, min: 0n, bits: 64 },
  maxPriorityFeePerGas: { max: U128_MAX, min: 0n, bits: 128 },
  maxFeePerGas: { max: U128_MAX, min: 0n, bits: 128 },
  gas: { max: U64_MAX, min: 0n, bits: 64 },
  value: { max: U128_MAX, min: 0n, bits: 128 },
};

/** Edge values for a field: 0, 1, the single-byte RLP boundaries, each byte-width boundary, the top bit, and the maximum. */
function edgeValues(field) {
  const { bits, max, min } = FIELDS[field];
  const edges = [['0', 0n], ['1', 1n], ['127', 127n], ['128', 128n], ['255', 255n], ['256', 256n]];
  for (let width = 8; width < bits; width += 8) {
    edges.push([`2^${width}-1`, 2n ** BigInt(width) - 1n]);
    edges.push([`2^${width}`, 2n ** BigInt(width)]);
  }
  edges.push([`2^${bits - 1}`, 2n ** BigInt(bits - 1)]);
  edges.push(['max', max]);
  const seen = new Set();
  return edges.filter(([, value]) => value >= min && value <= max && !seen.has(value) && seen.add(value));
}

/** Sets one field and keeps the tip at or below the fee cap. */
function withField(tx, field, value) {
  const next = { ...tx, [field]: value };
  if (field === 'maxFeePerGas' && next.maxPriorityFeePerGas > value) next.maxPriorityFeePerGas = value;
  if (field === 'maxPriorityFeePerGas' && next.maxFeePerGas < value) next.maxFeePerGas = value;
  return next;
}

// ---- Cases ----

const privateKey = keccak256(toBytes('minder evm corpus test key v1'));
const cases = [];

const asBase = (tx) => ({ ...tx, type: 'eip1559', data: tx.data ?? '0x', accessList: [] });

function record(name, kind, tx, signature) {
  const base = asBase(tx);
  const unsigned = serializeTransaction(base);
  return {
    name,
    kind,
    tx: {
      chain_id: tx.chainId.toString(),
      nonce: tx.nonce.toString(),
      max_priority_fee_per_gas: tx.maxPriorityFeePerGas.toString(),
      max_fee_per_gas: tx.maxFeePerGas.toString(),
      gas_limit: tx.gas.toString(),
      to: tx.to,
      value: tx.value.toString(),
      data: tx.data ?? '0x',
    },
    unsigned,
    signing_hash: keccak256(unsigned),
    signature: { r: signature.r, s: signature.s, y_parity: signature.yParity, source: signature.source },
    signed: serializeTransaction(base, { r: signature.r, s: signature.s, yParity: signature.yParity }),
  };
}

/** The same signature in high-s form: s becomes n - s and the parity flips. */
function highSMirror(signature) {
  if (signature.yParity !== 0 && signature.yParity !== 1) throw new Error('highSMirror needs a signature with yParity 0 or 1');
  const s = BigInt(signature.s);
  if (s > HALF) throw new Error('only a low-s signature has a high-s mirror');
  return { s: hex(N - s), y_parity: signature.yParity ^ 1 };
}

/** A real, deterministic, canonical (low-s) viem signature over the transaction's signing hash. */
async function realSignature(tx) {
  const hash = keccak256(serializeTransaction(asBase(tx)));
  const { r, s, yParity } = await sign({ hash, privateKey });
  return { r, s, yParity, source: 'viem-sign' };
}

async function addSigned(name, kind, tx, { mirror }) {
  const signature = await realSignature(tx);
  const entry = record(name, kind, tx, signature);
  if (mirror) entry.high_s = highSMirror(signature);
  cases.push(entry);
}

function addSynthetic(name, kind, tx, r, s, yParity, { mirror }) {
  const signature = { r: hex(r), s: hex(s), yParity, source: 'synthetic' };
  const entry = record(name, kind, tx, signature);
  if (mirror) entry.high_s = highSMirror(signature);
  cases.push(entry);
}

// 1. Random: every field across its full range, uniform in bit length.
const RANDOM_CASES = 1000;
for (let i = 0; i < RANDOM_CASES; i++) {
  await addSigned(`random/${String(i).padStart(4, '0')}`, 'random', randomTx(), { mirror: i % 2 === 0 });
}

// 2. Boundaries, one field at a time, the rest random.
for (const field of Object.keys(FIELDS)) {
  for (const [label, value] of edgeValues(field)) {
    await addSigned(`boundary/${field}/${label}`, 'boundary-field', withField(randomTx(), field, value), { mirror: true });
  }
}

// 3. Boundaries, every field at once.
const allMin = { chainId: 1n, nonce: 0n, maxPriorityFeePerGas: 0n, maxFeePerGas: 0n, gas: 0n, to: ZERO_ADDRESS, value: 0n };
const allMax = { chainId: U64_MAX, nonce: U64_MAX, maxPriorityFeePerGas: U128_MAX, maxFeePerGas: U128_MAX, gas: U64_MAX, to: ONES_ADDRESS, value: U128_MAX };
await addSigned('boundary/all/min', 'boundary-all', allMin, { mirror: true });
await addSigned('boundary/all/max', 'boundary-all', allMax, { mirror: true });
await addSigned('boundary/all/min-with-max-address', 'boundary-all', { ...allMin, to: ONES_ADDRESS }, { mirror: true });
await addSigned('boundary/all/max-with-zero-address', 'boundary-all', { ...allMax, to: ZERO_ADDRESS }, { mirror: true });
await addSigned('boundary/all/tip-equals-fee-max', 'boundary-all', { ...randomTx(), maxPriorityFeePerGas: U128_MAX, maxFeePerGas: U128_MAX }, { mirror: true });
await addSigned('boundary/all/tip-zero-fee-max', 'boundary-all', { ...randomTx(), maxPriorityFeePerGas: 0n, maxFeePerGas: U128_MAX }, { mirror: true });

// 4. Address patterns.
const addresses = {
  zero: ZERO_ADDRESS,
  ones: ONES_ADDRESS,
  'low-bit': '0x' + '00'.repeat(19) + '01',
  'high-bit': '0x80' + '00'.repeat(19),
  'all-but-low-bit': '0x' + 'ff'.repeat(19) + 'fe',
  'leading-zero-bytes': '0x0000' + rng.bytes(18),
  'trailing-zero-bytes': '0x' + rng.bytes(18) + '0000',
  'single-byte-below-0x80': '0x7f' + rng.bytes(19),
};
for (const [label, to] of Object.entries(addresses)) {
  await addSigned(`boundary/to/${label}`, 'boundary-address', { ...randomTx(), to }, { mirror: false });
}

// 5. The RLP list-length boundary (a payload of 55 bytes is the longest short
//    list, 56 the first long one), aimed at the unsigned payload.
const PAYLOAD_TARGETS = [52, 53, 54, 55, 56, 57, 58];
const PER_TARGET = 16;
for (const target of PAYLOAD_TARGETS) {
  for (let found = 0; found < PER_TARGET; ) {
    const tx = randomTx();
    if (unsignedPayload(tx) !== target) continue;
    const entry = record(`boundary/unsigned-payload/${target}/${found}`, 'boundary-list-length', tx, await realSignature(tx));
    // Self-check the label against the bytes: 0xc0+len for a short list, 0xf8 len for a long one.
    const bytes = Buffer.from(entry.unsigned.slice(2), 'hex');
    const header = target <= 55 ? [0xc0 + target] : [0xf8, target];
    if (!header.every((byte, i) => bytes[1 + i] === byte)) throw new Error(`payload ${target} header mismatch`);
    entry.high_s = highSMirror({ s: entry.signature.s, yParity: entry.signature.y_parity });
    cases.push(entry);
    found++;
  }
}

// 6. The same boundary in the *signed* encoding, which needs a signature small
//    enough for the signed payload to land near 55: synthetic r and s of a few bytes.
const SIGNED_TARGETS = [53, 54, 55, 56, 57, 58];
const PER_SIGNED_TARGET = 10;
for (const target of SIGNED_TARGETS) {
  for (let found = 0; found < PER_SIGNED_TARGET; ) {
    const tx = randomTx();
    const r = rng.bits(40) || 1n;
    const s = rng.bits(40) || 1n;
    const yParity = rng.int(0, 1);
    if (unsignedPayload(tx) + 1 + uintSize(r) + uintSize(s) !== target) continue;
    const before = cases.length;
    addSynthetic(`boundary/signed-payload/${target}/${found}`, 'boundary-signed-list-length', tx, r, s, yParity, { mirror: true });
    const bytes = Buffer.from(cases[before].signed.slice(2), 'hex');
    const header = target <= 55 ? [0xc0 + target] : [0xf8, target];
    if (!header.every((byte, i) => bytes[1 + i] === byte)) throw new Error(`signed payload ${target} header mismatch`);
    found++;
  }
}

// 7. Low-s normalization against the reference. Each case records viem's
//    serialization of the canonical signature and also the high-s form of the
//    same signature (n - s, parity flipped), which the contract must map back
//    to those exact bytes. The edges are the ones that decide `s > n/2`.
const lowS = {
  'one': 1n,
  'two': 2n,
  'half-minus-one': HALF - 1n,
  'half': HALF, // the largest low s; its mirror (half + 1) is the smallest high s
};
const rValues = {
  'r-one': 1n,
  'r-order-minus-one': N - 1n,
  'r-top-bit': 2n ** 255n,
};
for (const [sLabel, s] of Object.entries(lowS)) {
  for (const [rLabel, r] of Object.entries(rValues)) {
    for (const yParity of [0, 1]) {
      addSynthetic(`normalize/s-${sLabel}/${rLabel}/y${yParity}`, 'normalize-edge', randomTx(), r, s, yParity, { mirror: true });
    }
  }
}

// 8. ERC-20 transfer calls, as request_evm_token_payment builds them: to the
//    token, zero value, the 68-byte transfer calldata. Every other field random.
const transferData = (recipient, amount) => encodeFunctionData({ abi: erc20Abi, functionName: 'transfer', args: [recipient, amount] });
const TOKEN_CASES = 300;
for (let i = 0; i < TOKEN_CASES; i++) {
  const tx = { ...randomTx(), value: 0n };
  const recipient = '0x' + rng.bytes(20);
  const amount = rng.bits(128);
  await addSigned(`token/random/${String(i).padStart(4, '0')}`, 'token-transfer', { ...tx, data: transferData(recipient, amount) }, { mirror: i % 2 === 0 });
}

// 9. Token transfers at the edges: every amount width boundary (the calldata
//    word is always 32 bytes, so the data length never changes), and recipients
//    at the address extremes.
const amountEdges = [0n, 1n, 127n, 128n, 255n, 256n, 2n ** 64n - 1n, 2n ** 64n, 2n ** 127n, U128_MAX];
const recipientEdges = { zero: ZERO_ADDRESS, ones: ONES_ADDRESS, 'low-bit': '0x' + '00'.repeat(19) + '01', 'high-bit': '0x80' + '00'.repeat(19) };
for (const [rLabel, recipient] of Object.entries(recipientEdges)) {
  for (const amount of amountEdges) {
    await addSigned(`token/edge/${rLabel}/${amount}`, 'token-transfer-edge', { ...randomTx(), value: 0n, data: transferData(recipient, amount) }, { mirror: false });
  }
}

// 10. Data at the RLP string-length edges: empty, one byte below 0x80 (its own
//     encoding), 0x80 (not), 55 and 56 bytes (the short and long string forms),
//     and longer, with the transaction otherwise random.
const dataBytes = (length, seed) => '0x' + Array.from({ length }, (_, i) => ((i * 7 + seed) & 0xff).toString(16).padStart(2, '0')).join('');
const dataLengths = [0, 1, 2, 54, 55, 56, 57, 100, 255, 256, 1000];
for (const length of dataLengths) {
  for (const seed of [1, 0x7f]) {
    await addSigned(`data/length-${length}/seed-${seed}`, 'data-length', { ...randomTx(), data: dataBytes(length, seed) }, { mirror: false });
  }
}
for (const [label, data] of [['0x00', '0x00'], ['0x7f', '0x7f'], ['0x80', '0x80'], ['0xff', '0xff']]) {
  await addSigned(`data/single-byte/${label}`, 'data-single-byte', { ...randomTx(), data }, { mirror: false });
}

// ---- Output ----

const kinds = {};
for (const entry of cases) kinds[entry.kind] = (kinds[entry.kind] ?? 0) + 1;

const header = {
  description:
    'Differential corpus for src/evm.rs. For every case: viem\'s unsigned serialization, its keccak signing hash, and its signed serialization. ' +
    'Cases with high_s also give the high-s form of the same signature (s = n - s, y_parity flipped); the contract must reproduce the canonical signed bytes from it.',
  tx_shape: 'EIP-1559, empty access list: value transfers with empty data, ERC-20 transfer calls, and data at the RLP length edges',
  viem: PINNED_VIEM,
  regenerate:
    'In a directory with `npm install viem@2.57.16`: node <repo>/contract/vectors/gen-evm-corpus.mjs > <repo>/contract/vectors/evm-corpus.json',
  prng: 'SplitMix64',
  seed: '0x' + SEED.toString(16),
  signing_key:
    'keccak256 of the string "minder evm corpus test key v1": derived from a public string, controls no funds, not a secret. Signatures are viem sign() with RFC 6979 nonces, so they are deterministic and low-s.',
  encoding: 'numbers are decimal strings; byte strings are 0x-prefixed lowercase hex; r and s are 32-byte padded; y_parity is 0 or 1',
  tip_not_above_fee: 'every transaction satisfies max_priority_fee_per_gas <= max_fee_per_gas and chain_id >= 1, as viem and the contract both require',
  case_count: cases.length,
  kinds,
};

const lines = ['{', `"header": ${JSON.stringify(header, null, 2)},`, '"cases": ['];
cases.forEach((entry, i) => lines.push('  ' + JSON.stringify(entry) + (i < cases.length - 1 ? ',' : '')));
lines.push(']', '}');
process.stdout.write(lines.join('\n') + '\n');
