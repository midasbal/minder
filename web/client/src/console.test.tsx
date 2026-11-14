import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, readdirSync } from 'node:fs';
import { describe, it } from 'node:test';

import { renderToString } from 'react-dom/server';

import { AgentPanel } from './components/console/AgentPanel';
import { LivePanel } from './components/console/LivePanel';
import { prettyJson } from './components/console/RawCall';
import { Verdict } from './components/console/Verdict';
import { AGENT_PUBLIC_KEY, recipientError } from './chain/assets';
import { callView, dryRun, parseDecision } from './chain/contract';
import { presetsFor } from './chain/presets';
import { reasonLine } from './chain/reasons';
import { RpcFailure, createRpc, type Rpc } from './chain/rpc';
import { readSnapshot, reachedChain } from './chain/snapshot';
import { createConsoleStore } from './chain/store';
import { formatUnits, parseUnits } from './chain/units';

/* ---------- a stand-in for the chain: the shapes the live contract and node return ---------- */

const NEAR = 10n ** 24n;
const USDC_TOKEN = [28, 125, 75, 25, 108, 176, 199, 176, 29, 116, 63, 188, 97, 22, 169, 2, 55, 156, 114, 56];
const VENDOR = 'vendor.minder-gate-owner-bf5fcd.testnet';
const EVM_VENDOR = '0xe3390e9d6fc9456b7e7cdf4678d918dd4a2870d5';
const CODE = Buffer.from('a stand-in for the contract wasm');
const CODE_SHA256 = createHash('sha256').update(CODE).digest('hex');

const POLICIES = [
  ['Near', { per_tx_cap: String(NEAR / 2n), window_limit: String(NEAR), epoch_budget: String(2n * NEAR) }],
  [{ Evm: 11155111 }, { per_tx_cap: '5000000000000000', window_limit: '15000000000000000', epoch_budget: '30000000000000000' }],
  [{ Evm20: { chain_id: 11155111, token: USDC_TOKEN } }, { per_tx_cap: '5000000', window_limit: '10000000', epoch_budget: null }],
];

type Answers = Record<string, (args: Record<string, unknown>) => unknown>;

const policyOf = (asset: unknown) => POLICIES.find(([candidate]) => JSON.stringify(candidate) === JSON.stringify(asset))![1] as {
  per_tx_cap: string;
  window_limit: string;
  epoch_budget: string | null;
};

function chainAnswers(): Answers {
  return {
    get_config: () => ({
      owner: 'owner.testnet',
      status: 'Active',
      window_len: '60000000000',
      num_buckets: 6,
      timelock_delay: '120000000000',
      guardian: 'guardian.testnet',
    }),
    get_status: () => 'Active',
    get_mandate: () => ({
      agent_key: AGENT_PUBLIC_KEY,
      expiry: '1797231443760942299',
      epoch: 2,
      revoked: false,
      scope: {
        allowed_assets: ['Near', { Evm: 11155111 }, { Evm20: { chain_id: 11155111, token: USDC_TOKEN } }],
        allowed_near_recipients: [VENDOR],
        allowed_evm_recipients: [EVM_VENDOR],
        per_tx_cap: String(NEAR),
      },
    }),
    get_asset_policies: () => POLICIES,
    get_recipients: () => [VENDOR],
    get_evm_recipients: () => [EVM_VENDOR],
    get_headroom: ({ asset }) => {
      const policy = policyOf(asset);
      return { per_tx_cap: policy.per_tx_cap, window_remaining: policy.window_limit, budget_remaining: policy.epoch_budget };
    },
    get_budget_usage: ({ asset }) => ({ committed: String(BigInt(policyOf(asset).per_tx_cap) / 10n), reserved: '0', budget_epoch: '1' }),
    dry_run: () => 'Allow',
    dry_run_evm: () => 'Allow',
    dry_run_evm_token: () => ({ token: 'Allow', gas: 'Allow' }),
  };
}

interface FakeChain {
  rpc: Rpc;
  calls: Array<{ method: string; args: Record<string, unknown> }>;
}

function fakeChain(answers: Answers = chainAnswers(), extra: { keys?: unknown[]; code?: Buffer } = {}): FakeChain {
  const calls: FakeChain['calls'] = [];
  const keys = extra.keys ?? [
    {
      public_key: AGENT_PUBLIC_KEY,
      access_key: {
        nonce: 1,
        permission: {
          FunctionCall: {
            allowance: '246798533699992300000000',
            receiver_id: 'gate.minder-gate-owner-bf5fcd.testnet',
            method_names: ['request_local_transfer', 'request_evm_payment', 'request_evm_token_payment'],
          },
        },
      },
    },
  ];
  const trace = (request: unknown, response: unknown) => ({ endpoint: 'https://node.test', request, response, milliseconds: 3 });
  const rpc: Rpc = {
    async query(params) {
      const request = { jsonrpc: '2.0', id: 'minder', method: 'query', params };
      if (params.request_type === 'view_access_key_list') {
        const result = { keys, block_height: 100 };
        return { result, trace: trace(request, { result }) };
      }
      if (params.request_type === 'view_code') {
        const result = { code_base64: (extra.code ?? CODE).toString('base64'), hash: 'x', block_height: 101 };
        return { result, trace: trace(request, { result }) };
      }
      const method = params.method_name as string;
      const args = JSON.parse(Buffer.from(params.args_base64 as string, 'base64').toString()) as Record<string, unknown>;
      calls.push({ method, args });
      const answer = answers[method];
      if (!answer) throw new RpcFailure('contract', `no such method ${method}`);
      const result = { result: [...Buffer.from(JSON.stringify(answer(args)))], logs: [], block_height: 102 };
      return { result, trace: trace(request, { result }) };
    },
  };
  return { rpc, calls };
}

const failing = (message: string): Rpc => ({
  async query() {
    throw new RpcFailure('unreachable', message);
  },
});

/* ---------- units and input ---------- */

describe('units', () => {
  it('turns decimal text into exact base units, with no floating point', () => {
    assert.deepEqual(parseUnits('0.05', 24, 'NEAR'), { ok: true, value: 50_000_000_000_000_000_000_000n });
    assert.deepEqual(parseUnits('0.0002', 18, 'ETH'), { ok: true, value: 200_000_000_000_000n });
    assert.deepEqual(parseUnits('0.5', 6, 'USDC'), { ok: true, value: 500_000n });
    assert.deepEqual(parseUnits(' 12 ', 6, 'USDC'), { ok: true, value: 12_000_000n });
    assert.deepEqual(parseUnits('0.1', 18, 'ETH'), { ok: true, value: 100_000_000_000_000_000n });
  });

  it('refuses what is not an amount, or has more precision than the asset', () => {
    for (const bad of ['', 'abc', '-1', '1e3', '1.', '.5', '1,5', '0x10']) assert.equal(parseUnits(bad, 6, 'USDC').ok, false, bad);
    const tooFine = parseUnits('0.0000001', 6, 'USDC');
    assert.equal(tooFine.ok, false);
    assert.match(tooFine.ok ? '' : tooFine.error, /6 decimal places/);
    assert.equal(parseUnits(String(2n ** 128n), 0, 'x').ok, false, 'larger than u128');
    assert.equal(parseUnits(String(2n ** 128n - 1n), 0, 'x').ok, true);
  });

  it('formats back without trailing zeros, and round-trips', () => {
    assert.equal(formatUnits(50_000_000_000_000_000_000_000n, 24), '0.05');
    assert.equal(formatUnits(NEAR, 24), '1');
    assert.equal(formatUnits(500_000n, 6), '0.5');
    assert.equal(formatUnits(1n, 6), '0.000001');
    for (const text of ['0.05', '123.456789', '7']) {
      const parsed = parseUnits(text, 24, 'NEAR');
      assert.ok(parsed.ok);
      assert.equal(formatUnits(parsed.value, 24), text);
    }
  });

  it('checks the shape of a recipient and leaves the judgement to the contract', () => {
    assert.equal(recipientError('near', VENDOR), null);
    assert.equal(recipientError('near', 'not-on-the-list.testnet'), null);
    assert.notEqual(recipientError('near', 'Has Spaces'), null);
    assert.notEqual(recipientError('near', ''), null);
    assert.equal(recipientError('evm', EVM_VENDOR), null);
    assert.equal(recipientError('evm', '0x000000000000000000000000000000000000dEaD'), null);
    assert.notEqual(recipientError('evm', '0x1234'), null);
    assert.notEqual(recipientError('evm', 'e3390e9d6fc9456b7e7cdf4678d918dd4a2870d5'), null);
  });
});

/* ---------- the RPC layer ---------- */

const ok = (result: unknown) => new Response(JSON.stringify({ jsonrpc: '2.0', id: 'minder', result }), { status: 200 });

describe('the RPC client', () => {
  it('moves to the next node when one is unreachable, and remembers the one that worked', async () => {
    const seen: string[] = [];
    const rpc = createRpc({
      endpoints: ['https://a.test', 'https://b.test'],
      fetchImpl: (async (url: string) => {
        seen.push(url);
        if (url.startsWith('https://a.test')) throw new TypeError('network down');
        return ok({ fine: true });
      }) as unknown as typeof fetch,
    });
    assert.deepEqual((await rpc.query({})).result, { fine: true });
    assert.deepEqual(seen, ['https://a.test', 'https://b.test']);
    await rpc.query({});
    assert.deepEqual(seen.slice(2), ['https://b.test'], 'goes straight to the node that last answered');
  });

  it('treats a non-2xx answer and a body that is not JSON as an unreachable node', async () => {
    const answers = [new Response('slow down', { status: 429 }), new Response('<html>', { status: 200 })];
    const rpc = createRpc({
      endpoints: ['https://a.test', 'https://b.test', 'https://c.test'],
      fetchImpl: (async (url: string) => (url.includes('c.test') ? ok(1) : answers.shift()!)) as unknown as typeof fetch,
    });
    assert.equal((await rpc.query({})).result, 1);
  });

  it('gives up with a clear failure naming each node when none answers', async () => {
    const rpc = createRpc({
      endpoints: ['https://a.test', 'https://b.test'],
      fetchImpl: (async () => new Response('', { status: 503 })) as unknown as typeof fetch,
    });
    await assert.rejects(rpc.query({}), (error: RpcFailure) => {
      assert.equal(error.kind, 'unreachable');
      assert.match(error.message, /Could not reach the NEAR RPC/);
      assert.match(error.message, /a\.test: HTTP 503/);
      assert.match(error.message, /b\.test: HTTP 503/);
      return true;
    });
  });

  it('times out a node that never answers and tries the next', async () => {
    const rpc = createRpc({
      endpoints: ['https://slow.test', 'https://fast.test'],
      timeoutMs: 20,
      fetchImpl: ((url: string, init: RequestInit) =>
        url.includes('slow')
          ? new Promise((_, reject) => init.signal!.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError'))))
          : Promise.resolve(ok('there'))) as unknown as typeof fetch,
    });
    assert.equal((await rpc.query({})).result, 'there');
    const lonely = createRpc({
      endpoints: ['https://slow.test'],
      timeoutMs: 20,
      fetchImpl: ((_: string, init: RequestInit) =>
        new Promise((__, reject) => init.signal!.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError'))))) as unknown as typeof fetch,
    });
    await assert.rejects(lonely.query({}), (error: RpcFailure) => error.kind === 'timeout' && /no answer within/.test(error.message));
  });

  it("does not shop around for a different answer when the chain itself says no", async () => {
    let asked = 0;
    const rpc = createRpc({
      endpoints: ['https://a.test', 'https://b.test'],
      fetchImpl: (async () => {
        asked += 1;
        return new Response(JSON.stringify({ error: { name: 'HANDLER_ERROR', cause: { name: 'UNKNOWN_ACCOUNT' } } }), { status: 200 });
      }) as unknown as typeof fetch,
    });
    await assert.rejects(rpc.query({}), (error: RpcFailure) => error.kind === 'rpc' && /UNKNOWN_ACCOUNT/.test(error.message));
    assert.equal(asked, 1);
  });

  it('does move on when a node reports it could not serve the request', async () => {
    const rpc = createRpc({
      endpoints: ['https://a.test', 'https://b.test'],
      fetchImpl: (async (url: string) =>
        url.includes('a.test')
          ? new Response(JSON.stringify({ error: { name: 'HANDLER_ERROR', cause: { name: 'UNAVAILABLE_SHARD' } } }), { status: 200 })
          : ok('served')) as unknown as typeof fetch,
    });
    assert.equal((await rpc.query({})).result, 'served');
  });

  it('keeps only a few requests in flight at once', async () => {
    let live = 0;
    let peak = 0;
    const rpc = createRpc({
      endpoints: ['https://a.test'],
      maxConcurrent: 3,
      fetchImpl: (async () => {
        live += 1;
        peak = Math.max(peak, live);
        await new Promise((resolve) => setTimeout(resolve, 5));
        live -= 1;
        return ok(0);
      }) as unknown as typeof fetch,
    });
    await Promise.all(Array.from({ length: 12 }, () => rpc.query({})));
    assert.equal(peak, 3);
  });

  it('sends one well-formed JSON-RPC query and keeps the exchange for inspection', async () => {
    let body: unknown;
    const rpc = createRpc({
      endpoints: ['https://a.test'],
      fetchImpl: (async (_: string, init: RequestInit) => {
        body = JSON.parse(init.body as string);
        return ok({ answer: 1 });
      }) as unknown as typeof fetch,
    });
    const { trace } = await rpc.query({ request_type: 'view_code' });
    assert.deepEqual(body, { jsonrpc: '2.0', id: 'minder', method: 'query', params: { request_type: 'view_code' } });
    assert.deepEqual(trace.request, body);
    assert.deepEqual(trace.response, { jsonrpc: '2.0', id: 'minder', result: { answer: 1 } });
    assert.equal(trace.endpoint, 'https://a.test');
  });
});

/* ---------- the dry run ---------- */

describe('the dry run', () => {
  it('asks the live contract through the matching view, as the agent, never with a secret', async () => {
    const chain = fakeChain();
    await dryRun(chain.rpc, { method: 'request_local_transfer', recipient: ` ${VENDOR} `, amount: 50_000_000_000_000_000_000_000n });
    await dryRun(chain.rpc, { method: 'request_evm_payment', recipient: EVM_VENDOR, amount: 200_000_000_000_000n });
    await dryRun(chain.rpc, { method: 'request_evm_token_payment', recipient: EVM_VENDOR, amount: 500_000n, token: USDC_TOKEN });
    assert.deepEqual(chain.calls, [
      { method: 'dry_run', args: { agent_key: AGENT_PUBLIC_KEY, asset: 'Near', amount: '50000000000000000000000', recipient: VENDOR } },
      { method: 'dry_run_evm', args: { agent_key: AGENT_PUBLIC_KEY, recipient: EVM_VENDOR, amount: '200000000000000' } },
      {
        method: 'dry_run_evm_token',
        args: { agent_key: AGENT_PUBLIC_KEY, token: '0x1c7d4b196cb0c7b01d743fbc6116a902379c7238', recipient: EVM_VENDOR, amount: '500000' },
      },
    ]);
  });

  it("returns the contract's own verdict and reason, whatever it is", async () => {
    for (const reason of ['PerTxCapExceeded', 'RecipientNotAllowed', 'WindowLimitExceeded', 'RecipientOutOfScope']) {
      const chain = fakeChain({ ...chainAnswers(), dry_run: () => ({ Deny: reason }) });
      const result = await dryRun(chain.rpc, { method: 'request_local_transfer', recipient: VENDOR, amount: 1n });
      assert.equal(result.allowed, false);
      assert.deepEqual(result.legs, [{ name: 'payment', verdict: { allow: false, reason } }]);
    }
    const allow = await dryRun(fakeChain().rpc, { method: 'request_local_transfer', recipient: VENDOR, amount: 1n });
    assert.equal(allow.allowed, true);
  });

  it('judges a token payment by both of its legs, and names the one that refused', async () => {
    const chain = fakeChain({ ...chainAnswers(), dry_run_evm_token: () => ({ token: 'Allow', gas: { Deny: 'PerTxCapExceeded' } }) });
    const result = await dryRun(chain.rpc, { method: 'request_evm_token_payment', recipient: EVM_VENDOR, amount: 1n, token: USDC_TOKEN });
    assert.equal(result.allowed, false);
    assert.deepEqual(
      result.legs.map((leg) => [leg.name, leg.verdict.allow]),
      [['token', true], ['gas', false]],
    );
  });

  it('refuses a token payment with no token, and a decision it does not recognise, rather than guess', async () => {
    await assert.rejects(dryRun(fakeChain().rpc, { method: 'request_evm_token_payment', recipient: EVM_VENDOR, amount: 1n }), RpcFailure);
    assert.throws(() => parseDecision('Maybe'), RpcFailure);
    assert.throws(() => parseDecision({ Deny: 7 }), RpcFailure);
    assert.throws(() => parseDecision(null), RpcFailure);
  });

  it("reports a contract panic as the contract's, and a dead node as unreachable", async () => {
    const panicking: Rpc = {
      async query() {
        return { result: { error: 'wasm execution failed: Smart contract panicked: bad address', logs: [] }, trace: { endpoint: 'x', request: {}, response: {}, milliseconds: 1 } };
      },
    };
    await assert.rejects(callView(panicking, 'dry_run_evm'), (error: RpcFailure) => error.kind === 'contract' && /bad address/.test(error.message));
    await assert.rejects(dryRun(failing('down'), { method: 'request_local_transfer', recipient: VENDOR, amount: 1n }), /down/);
  });

  it('keeps the call that was made, so the page can show it', async () => {
    const result = await dryRun(fakeChain().rpc, { method: 'request_local_transfer', recipient: VENDOR, amount: 1n });
    assert.equal(result.trace.method, 'dry_run');
    assert.equal((result.trace.args as { agent_key: string }).agent_key, AGENT_PUBLIC_KEY);
    assert.equal((result.trace.request as { method: string }).method, 'query');
  });
});

describe('the reasons', () => {
  it("describe every deny code the contract can return, with no gaps", () => {
    const source = readFileSync(new URL('../../../contract/src/types.rs', import.meta.url), 'utf8');
    const body = /pub enum DenyReason \{([\s\S]*?)\n\}/.exec(source)![1]!;
    const codes = body
      .split('\n')
      .map((line) => /^\s{4}([A-Z][A-Za-z]+),/.exec(line)?.[1])
      .filter((code): code is string => code !== undefined);
    assert.ok(codes.length >= 12, `found ${codes.length} codes`);
    for (const code of codes) assert.ok(!reasonLine(code).startsWith('The contract returned a reason'), code);
  });

  it('admits a reason it has no line for, and does not invent one', () => {
    assert.match(reasonLine('SomethingNew'), /no description for/);
  });
});

/* ---------- the live reading ---------- */

describe('the live reading', () => {
  it('reads every section from the chain and shapes it for display', async () => {
    const snapshot = await readSnapshot(fakeChain().rpc, { now: () => 1_700_000_000_000, expectedWasmSha256: CODE_SHA256 });
    assert.equal(snapshot.readAt, 1_700_000_000_000);
    assert.equal(snapshot.blockHeight, 102);
    assert.equal(snapshot.endpoint, 'https://node.test');
    assert.ok(snapshot.keys.ok && snapshot.code.ok && snapshot.assets.ok && snapshot.mandate.ok && snapshot.contract.ok);

    assert.deepEqual(snapshot.keys.ok && { full: snapshot.keys.value.fullAccess, restricted: snapshot.keys.value.restricted }, {
      full: 0,
      restricted: 1,
    });
    const key = snapshot.keys.ok ? snapshot.keys.value.keys[0]! : null;
    assert.equal(key?.publicKey, AGENT_PUBLIC_KEY);
    assert.equal(key?.allowance, '0.2467985336999923 NEAR');

    assert.equal(snapshot.contract.ok && snapshot.contract.value.windowSeconds, 60);
    assert.equal(snapshot.contract.ok && snapshot.contract.value.timelockSeconds, 120);

    const rows = snapshot.assets.ok ? snapshot.assets.value : [];
    assert.deepEqual(rows.map((row) => [row.label, row.perTx, row.window, row.budget]), [
      ['NEAR', '0.5 NEAR', '1 NEAR', '2 NEAR'],
      ['ETH on Sepolia', '0.005 ETH', '0.015 ETH', '0.03 ETH'],
      ['USDC on Sepolia', '5 USDC', '10 USDC', null],
    ]);
    const near = rows[0]!;
    assert.ok(near.headroom.ok && near.usage.ok);
    assert.equal(near.headroom.value.windowRemaining, '1 NEAR');
    assert.equal(near.usage.value.committed, '0.05 NEAR');
    const usdc = rows[2]!;
    assert.ok(usdc.headroom.ok && usdc.usage.ok);
    assert.equal(usdc.headroom.value.windowRemaining, '10 USDC');
    assert.equal(usdc.headroom.value.budgetRemaining, null, 'a policy with no budget has no budget headroom');
    assert.equal(usdc.usage.value.committed, '0.5 USDC');

    assert.equal(snapshot.mandate.ok && snapshot.mandate.value?.expiry, '2026-12-14');
    assert.deepEqual(snapshot.nearRecipients, { ok: true, value: [VENDOR] });
    assert.deepEqual(snapshot.evmRecipients, { ok: true, value: [EVM_VENDOR] });
  });

  it('hashes the deployed bytecode in the browser and compares it with the repository build', async () => {
    const same = await readSnapshot(fakeChain().rpc, { expectedWasmSha256: CODE_SHA256 });
    assert.deepEqual(same.code, { ok: true, value: { bytes: CODE.length, sha256: CODE_SHA256, expected: CODE_SHA256, matches: true } });
    const different = await readSnapshot(fakeChain().rpc, { expectedWasmSha256: '0'.repeat(64) });
    assert.ok(different.code.ok);
    assert.equal(different.code.value.matches, false);
    assert.equal(different.code.value.sha256, CODE_SHA256, 'the hash shown is the chain\'s, not the expected one');
  });

  it('counts a full-access key as one, so a keyed contract is not reported as keyless', async () => {
    const keys = [
      { public_key: 'ed25519:owner', access_key: { nonce: 1, permission: 'FullAccess' } },
      {
        public_key: 'ed25519:agent',
        access_key: { nonce: 1, permission: { FunctionCall: { allowance: null, receiver_id: 'c.testnet', method_names: ['m'] } } },
      },
    ];
    const snapshot = await readSnapshot(fakeChain(chainAnswers(), { keys }).rpc);
    assert.ok(snapshot.keys.ok);
    assert.equal(snapshot.keys.value.fullAccess, 1);
    assert.equal(snapshot.keys.value.restricted, 1);
    assert.equal(snapshot.keys.value.keys[0]!.allowance, 'unlimited');
  });

  it('leaves a message where a call failed and keeps the rest', async () => {
    const answers = chainAnswers();
    answers.get_mandate = () => {
      throw new RpcFailure('contract', 'boom');
    };
    const snapshot = await readSnapshot(fakeChain(answers).rpc);
    assert.deepEqual(snapshot.mandate, { ok: false, message: 'boom' });
    assert.ok(snapshot.keys.ok && snapshot.assets.ok && snapshot.code.ok);
    assert.equal(reachedChain(snapshot), true);
  });

  it('shows a per-asset failure without blanking the policy', async () => {
    const answers = chainAnswers();
    answers.get_headroom = () => null;
    const snapshot = await readSnapshot(fakeChain(answers).rpc);
    assert.ok(snapshot.assets.ok);
    assert.equal(snapshot.assets.value[0]!.headroom.ok, false);
    assert.equal(snapshot.assets.value[0]!.perTx, '0.5 NEAR');
  });

  it('says it reached nothing when every call fails', async () => {
    const snapshot = await readSnapshot(failing('Could not reach the NEAR RPC (a.test: HTTP 503)'));
    assert.equal(reachedChain(snapshot), false);
    for (const section of [snapshot.contract, snapshot.mandate, snapshot.assets, snapshot.keys, snapshot.code]) {
      assert.equal(section.ok, false);
    }
  });

  it('makes a bounded number of calls for a whole reading', async () => {
    const chain = fakeChain();
    await readSnapshot(chain.rpc);
    // 2 for state, 1 mandate, 1 policies, 2 lists, 2 per asset (3 assets): 12 view calls, plus the key list and the code.
    assert.equal(chain.calls.length, 12);
  });
});

/* ---------- presets ---------- */

describe('the presets', () => {
  it('are worked out from the live policy, and fill the form without naming a verdict', async () => {
    const snapshot = await readSnapshot(fakeChain().rpc);
    const near = presetsFor('request_local_transfer', snapshot);
    assert.equal(near.unavailable, null);
    const byId = Object.fromEntries(near.presets.map((preset) => [preset.id, preset]));
    assert.equal(byId['over-cap']!.amount, '1', 'twice the 0.5 NEAR cap');
    assert.equal(byId['over-cap']!.recipient, VENDOR);
    assert.equal(byId['unlisted-recipient']!.recipient, 'not-on-the-list.testnet');
    assert.equal(byId['whole-window']!.amount, '1', 'the whole window limit');
    assert.equal(byId['in-policy']!.amount, '0.025', 'a twentieth of the cap');
    for (const preset of near.presets) assert.ok(!/allow|deny|refus|block/i.test(`${preset.label} ${preset.tries}`), preset.id);

    const eth = presetsFor('request_evm_payment', snapshot);
    assert.equal(eth.presets.find((preset) => preset.id === 'over-cap')!.amount, '0.01');
    assert.equal(eth.presets.find((preset) => preset.id === 'unlisted-recipient')!.recipient, '0x000000000000000000000000000000000000dEaD');
    assert.equal(eth.presets.find((preset) => preset.id === 'in-policy')!.recipient, EVM_VENDOR);

    const usdc = presetsFor('request_evm_token_payment', snapshot);
    assert.equal(usdc.presets.find((preset) => preset.id === 'in-policy')!.amount, '0.25');
  });

  it('every amount they set is one the form accepts', async () => {
    const snapshot = await readSnapshot(fakeChain().rpc);
    for (const [method, decimals, symbol] of [
      ['request_local_transfer', 24, 'NEAR'],
      ['request_evm_payment', 18, 'ETH'],
      ['request_evm_token_payment', 6, 'USDC'],
    ] as const) {
      for (const preset of presetsFor(method, snapshot).presets) assert.equal(parseUnits(preset.amount, decimals, symbol).ok, true, `${method} ${preset.id}`);
    }
  });

  it('say why there are none while the policy is unread, or unreadable, or the allowlist is empty', async () => {
    assert.match(presetsFor('request_local_transfer', null).unavailable ?? '', /Reading the live policy/);
    const broken = await readSnapshot(failing('down'));
    assert.match(presetsFor('request_local_transfer', broken).unavailable ?? '', /could not be read/);
    const answers = chainAnswers();
    answers.get_recipients = () => [];
    const empty = await readSnapshot(fakeChain(answers).rpc);
    assert.match(presetsFor('request_local_transfer', empty).unavailable ?? '', /allowlist is empty/);
  });
});

/* ---------- the store: caching and the actions ---------- */

describe('the store', () => {
  it('reads the chain once, shares a reading in progress, and reads again only when forced', async () => {
    const chain = fakeChain();
    const store = createConsoleStore(chain.rpc);
    await Promise.all([store.load(), store.load(), store.load()]);
    const once = chain.calls.length;
    assert.equal(once, 12);
    await store.load();
    assert.equal(chain.calls.length, once, 'a ready reading is kept');
    await store.load(true);
    assert.equal(chain.calls.length, once * 2, 'Refresh reads again');
  });

  it('keeps the old reading on screen while a refresh is in flight', async () => {
    const store = createConsoleStore(fakeChain().rpc);
    await store.load();
    const before = store.getState().snapshot;
    const refresh = store.load(true);
    assert.equal(store.getState().reading, 'loading');
    assert.equal(store.getState().snapshot, before);
    await refresh;
    assert.equal(store.getState().reading, 'ready');
  });

  it('does not read on a dry run: only the call asked for is made', async () => {
    const chain = fakeChain();
    const store = createConsoleStore(chain.rpc);
    await store.dryRun({ method: 'request_local_transfer', recipient: VENDOR, amount: 1n });
    assert.deepEqual(chain.calls.map((call) => call.method), ['dry_run']);
    const run = store.getState().run;
    assert.equal(run.phase, 'done');
  });

  it('records a failed dry run as a failure with its reason, not as a verdict', async () => {
    const store = createConsoleStore(failing('Could not reach the NEAR RPC (a.test: HTTP 503)'));
    await store.dryRun({ method: 'request_local_transfer', recipient: VENDOR, amount: 1n });
    const run = store.getState().run;
    assert.equal(run.phase, 'failed');
    assert.match(run.phase === 'failed' ? run.message : '', /Could not reach the NEAR RPC/);
  });

  it('turns an unexpected shape into a message, not a crash', async () => {
    const answers = chainAnswers();
    answers.dry_run = () => ({ Surprise: true });
    const store = createConsoleStore(fakeChain(answers).rpc);
    await store.dryRun({ method: 'request_local_transfer', recipient: VENDOR, amount: 1n });
    assert.equal(store.getState().run.phase, 'failed');
  });

  it('tells subscribers about each change, and stops when they leave', async () => {
    const store = createConsoleStore(fakeChain().rpc);
    let heard = 0;
    const leave = store.subscribe(() => {
      heard += 1;
    });
    await store.load();
    assert.equal(heard, 2, 'loading, then ready');
    leave();
    await store.load(true);
    assert.equal(heard, 2);
  });
});

/* ---------- what is drawn ---------- */

async function readyStore(answers: Answers = chainAnswers()) {
  const store = createConsoleStore(fakeChain(answers).rpc, () => Date.UTC(2026, 10, 14, 9, 30, 5));
  await store.load();
  return store;
}

const text = (html: string) => html.replace(/<!--[\s\S]*?-->/g, '').replace(/<[^>]+>/g, ' ').replace(/&#x27;/g, "'").replace(/&amp;/g, '&').replace(/\s+/g, ' ');

describe('the agent panel', () => {
  it('before the chain is read: says the presets wait, shows no verdict, and still offers the form', () => {
    const store = createConsoleStore(fakeChain().rpc);
    const html = renderToString(<AgentPanel store={store} state={store.getState()} />);
    assert.ok(text(html).includes('Reading the live policy…'));
    assert.ok(text(html).includes('No request yet'));
    assert.ok(!text(html).includes('ALLOW') && !text(html).includes('DENY'));
    for (const label of ['NEAR', 'ETH on Sepolia', 'USDC on Sepolia']) assert.ok(html.includes(label), label);
    assert.match(html, /<button type="submit" class="btn btn--primary">Ask the contract<\/button>/);
  });

  it('after the reading: offers the four presets, each with what it tries', async () => {
    const store = await readyStore();
    const html = text(renderToString(<AgentPanel store={store} state={store.getState()} />));
    for (const label of ['Over the cap', 'Unlisted recipient', 'Drain the window', 'In policy']) assert.ok(html.includes(label), label);
    assert.ok(html.includes('twice the per-transaction cap'));
    assert.ok(html.includes('only fills the form'));
    assert.ok(html.includes('one dry run reaches them only once spending has accumulated'));
  });

  it('says plainly that a dry run is free and sends nothing', () => {
    const store = createConsoleStore(fakeChain().rpc);
    const html = text(renderToString(<AgentPanel store={store} state={store.getState()} />));
    assert.ok(html.includes('A dry run is free and sends nothing'));
    assert.ok(html.includes('gate.minder-gate-owner-bf5fcd.testnet'));
  });
});

describe('the verdict', () => {
  const input = { method: 'request_local_transfer', recipient: VENDOR, amount: 2n * NEAR } as const;

  it("shows a denial as the firewall refusing, with the contract's code and a plain line", async () => {
    const answers = chainAnswers();
    answers.dry_run = () => ({ Deny: 'PerTxCapExceeded' });
    const store = createConsoleStore(fakeChain(answers).rpc);
    await store.dryRun(input);
    const html = renderToString(<Verdict run={store.getState().run} />);
    assert.match(html, /<span class="stamp stamp--deny">DENY<\/span>/);
    assert.ok(!html.includes('stamp--allow'));
    const words = text(html);
    assert.ok(words.includes('The firewall refused it.'));
    assert.ok(words.includes('PerTxCapExceeded'));
    assert.ok(words.includes('larger than the per-transaction cap'));
    assert.ok(words.includes('2 NEAR'), 'the request is shown as the agent made it, in whole units');
    assert.ok(words.includes('Raw RPC request and response'));
  });

  it('shows an allow as an allow, and says nothing was sent', async () => {
    const store = await readyStore();
    await store.dryRun(input);
    const html = renderToString(<Verdict run={store.getState().run} />);
    assert.match(html, /<span class="stamp stamp--allow">ALLOW<\/span>/);
    assert.ok(!html.includes('stamp--deny'));
    assert.ok(text(html).includes('no funds moved'));
  });

  it('shows both legs of a token payment', async () => {
    const answers = chainAnswers();
    answers.dry_run_evm_token = () => ({ token: 'Allow', gas: { Deny: 'RecipientOutOfScope' } });
    const store = createConsoleStore(fakeChain(answers).rpc);
    await store.dryRun({ method: 'request_evm_token_payment', recipient: EVM_VENDOR, amount: 500_000n, token: USDC_TOKEN });
    const words = text(renderToString(<Verdict run={store.getState().run} />));
    assert.ok(words.includes('Token transfer') && words.includes('ETH gas for the transfer'));
    assert.ok(words.includes('RecipientOutOfScope'));
    assert.ok(words.includes('DENY') && words.includes('ALLOW'));
  });

  it('shows the real request and the response as the node sent it', async () => {
    const store = await readyStore();
    await store.dryRun(input);
    const html = renderToString(<Verdict run={store.getState().run} />);
    assert.ok(html.includes('https://node.test'));
    assert.ok(html.includes('&quot;method&quot;: &quot;query&quot;'));
    assert.ok(html.includes('&quot;request_type&quot;: &quot;call_function&quot;'));
    assert.ok(html.includes('&quot;method_name&quot;: &quot;dry_run&quot;'));
  });

  it('says it got no answer, and shows no verdict, when the call failed', async () => {
    const store = createConsoleStore(failing('Could not reach the NEAR RPC (a.test: timed out)'));
    await store.dryRun(input);
    const html = renderToString(<Verdict run={store.getState().run} />);
    const words = text(html);
    assert.ok(words.includes("Couldn't get an answer from the contract"));
    assert.ok(words.includes('No verdict is shown because none was received'));
    assert.ok(!html.includes('stamp--allow') && !html.includes('stamp--deny'));
  });
});

describe('the live panel', () => {
  it('shows what the chain returned, labelled as read from the chain, with the time and block', async () => {
    const store = await readyStore();
    const words = text(renderToString(<LivePanel store={store} state={store.getState()} />));
    assert.ok(words.includes('Read from the chain at 09:30:05 UTC, block 102.'));
    assert.ok(words.includes('0 full-access keys'));
    assert.ok(words.includes('1 restricted key'));
    assert.ok(words.includes('May call only'));
    assert.ok(words.includes('request_evm_token_payment'));
    assert.ok(words.includes('Check on the explorer'));
    assert.ok(words.includes('Active'));
    assert.ok(words.includes('0.5 NEAR') && words.includes('0.005 ETH') && words.includes('5 USDC'));
    assert.ok(words.includes(VENDOR) && words.includes(EVM_VENDOR));
    assert.ok(words.includes('Refresh'));
  });

  it('marks the keyless count verified only when there really are no full-access keys', async () => {
    const keyless = await readyStore();
    assert.match(text(renderToString(<LivePanel store={keyless} state={keyless.getState()} />)), /0 full-access keys ?, 1 restricted key/);
    assert.match(renderToString(<LivePanel store={keyless} state={keyless.getState()} />), /<span class="is-verified">0<!-- --> full-access/);
    const keys = [{ public_key: 'ed25519:owner', access_key: { nonce: 1, permission: 'FullAccess' } }];
    const store = createConsoleStore(fakeChain(chainAnswers(), { keys }).rpc);
    await store.load();
    const html = renderToString(<LivePanel store={store} state={store.getState()} />);
    assert.ok(!html.includes('is-verified">1<!-- --> full-access'));
    assert.ok(text(html).includes('1 full-access key'));
  });

  it('shows the code hash from the chain, and says when it does not match', async () => {
    const store = await readyStore();
    const words = text(renderToString(<LivePanel store={store} state={store.getState()} />));
    assert.ok(words.includes(CODE_SHA256));
    assert.ok(words.includes('does NOT match'), 'the stand-in wasm is not the repository build, and the page says so');
  });

  it('says it could not read a section, in place, and shows the others', async () => {
    const answers = chainAnswers();
    answers.get_mandate = () => {
      throw new RpcFailure('contract', 'boom');
    };
    const store = await readyStore(answers);
    const words = text(renderToString(<LivePanel store={store} state={store.getState()} />));
    assert.ok(words.includes("Couldn't read this from the chain. boom"));
    assert.ok(words.includes('0 full-access keys'));
  });

  it('shows a clear retry state, and no data, when the RPC cannot be reached', async () => {
    const store = createConsoleStore(failing('Could not reach the NEAR RPC (a.test: HTTP 503)'));
    await store.load();
    const html = renderToString(<LivePanel store={store} state={store.getState()} />);
    const words = text(html);
    assert.ok(words.includes("Couldn't reach the NEAR RPC"));
    assert.ok(words.includes('Use Refresh to retry'));
    assert.ok(!words.includes('Read from the chain at'), 'no reading is claimed');
    assert.ok(!html.includes('live-dot'));
    assert.ok(!html.includes('is-verified'));
  });

  it('shows a loading line before the first reading', () => {
    const store = createConsoleStore(fakeChain().rpc);
    assert.ok(text(renderToString(<LivePanel store={store} state={store.getState()} />)).includes('Reading the contract from the chain'));
  });
});

describe('the raw call', () => {
  it('prints byte lists on one line and everything else indented', () => {
    assert.equal(prettyJson({ result: [34, 65, 108, 108], ok: true }), '{\n  "result": [34 65 108 108],\n  "ok": true\n}'.replace('34 65 108 108', '34, 65, 108, 108'));
  });
});

/* ---------- what must never be in the browser ---------- */

describe('the frontend holds no secret and has no other server', () => {
  const dirs = ['chain', 'components/console'];
  const sources = dirs.flatMap((dir) =>
    readdirSync(new URL(`./${dir}/`, import.meta.url))
      .filter((name) => /\.tsx?$/.test(name))
      .map((name) => ({ file: `${dir}/${name}`, code: readFileSync(new URL(`./${dir}/${name}`, import.meta.url), 'utf8') })),
  );

  it('names exactly one key, the agent\'s public key', () => {
    const literal = /ed25519:[A-Za-z0-9]{20,}/g;
    const found = sources.flatMap(({ file, code }) => (code.match(literal) ?? []).map((key) => `${file}:${key}`));
    assert.deepEqual(found, [`chain/assets.ts:${AGENT_PUBLIC_KEY}`]);
  });

  it('has no private key, seed, signing or credential handling', () => {
    for (const { file, code } of sources) {
      const stripped = code.replace(/\/\*[\s\S]*?\*\//g, '').replace(/^\s*\/\/.*$/gm, '');
      assert.ok(!/private[_ ]?key|secret|seed|mnemonic|credential|sign(ing|ature|Transaction)|KeyPair/i.test(stripped), file);
      assert.ok(!/localStorage|sessionStorage|document\.cookie|process\.env|import\.meta\.env/.test(stripped), file);
    }
  });

  it('talks to the chain only from rpc.ts, and to nothing of ours', () => {
    for (const { file, code } of sources) {
      const hasFetch = /\bfetch\(/.test(code);
      assert.equal(hasFetch, file === 'chain/rpc.ts', file);
      assert.ok(!/['"`]\/(api|health)/.test(code), `${file}: no relay call`);
    }
  });

  it('only ever asks the contract for views', () => {
    const calls = sources.flatMap(({ code }) => [...code.matchAll(/request_type: '([a-z_]+)'/g)].map((match) => match[1]));
    assert.ok(calls.length >= 3);
    for (const type of calls) assert.ok(['call_function', 'view_access_key_list', 'view_code'].includes(type!), type);
    const methods = readFileSync(new URL('./chain/contract.ts', import.meta.url), 'utf8');
    assert.ok(!/broadcast_tx|send_tx|functionCall|FunctionCall/.test(methods));
  });
});
