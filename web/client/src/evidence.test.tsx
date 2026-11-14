import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { describe, it } from 'node:test';

import { renderToString } from 'react-dom/server';
import { StaticRouter } from 'react-router';

import { App } from './App';
import { FREEZE_DRILL, RED_TEAM, parseFreezeDrill, parseRedTeam } from './chain/evidence';
import { nearTxUrl } from './chain/liverun';
import { FreezeDrill } from './components/console/FreezeDrill';
import { RedTeam } from './components/console/RedTeam';
import { elapsed, utcClock } from './components/console/time';

/*
  The red-team log and the freeze drill, from the two evidence files recorded against the live contract. The checks
  are on the recorded data itself (so the page can only say what the chain said) and on what the page shows of it.
*/

const read = (name: string) => JSON.parse(readFileSync(new URL(`./data/${name}`, import.meta.url), 'utf8')) as Record<string, unknown> & {
  onChainDenials: Array<Record<string, unknown>>;
  window: { entries: Array<Record<string, unknown>> };
  protocolRefusals: Array<Record<string, unknown>>;
  entries: Array<Record<string, unknown>>;
};
const redTeamFile = read('red-team.json');
const drillFile = read('freeze-drill.json');

const draw = (element: React.ReactElement) => renderToString(<StaticRouter location="/app">{element}</StaticRouter>);
const text = (html: string) =>
  html
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/<[^>]+>/g, ' ')
    .replace(/&#x27;/g, "'")
    .replace(/&quot;/g, '"')
    .replace(/&amp;/g, '&')
    .replace(/\u2019/g, "'")
    .replace(/\s+/g, ' ');
const count = (html: string, needle: string) => html.split(needle).length - 1;

describe('the recorded red-team run', () => {
  it('holds eight on-chain denials, the three-step window burst and five runtime refusals', () => {
    assert.equal(RED_TEAM.denials.length, 8);
    assert.deepEqual(RED_TEAM.window.map((step) => step.allowed), [true, true, false]);
    assert.equal(RED_TEAM.refusals.length, 5);
  });

  it('refused every attack with the reason it was meant to hit, and the contract\'s own event says the same', () => {
    for (const entry of redTeamFile.onChainDenials) {
      assert.equal(entry.reason, entry.expectedReason, String(entry.id));
      assert.equal(entry.finalStatus, 'Failure');
      const event = entry.decisionEvent as Record<string, unknown>;
      assert.equal(event.decision, 'deny');
      assert.equal(event.reason, entry.reason);
      assert.equal(event.method, entry.method);
    }
    assert.deepEqual(
      RED_TEAM.denials.map((d) => d.reason),
      ['PerTxCapExceeded', 'PerTxCapExceeded', 'PerTxCapExceeded', 'RecipientNotAllowed', 'RecipientNotAllowed', 'RecipientNotAllowed', 'AssetNotAllowed', 'AssetNotAllowed'],
    );
  });

  it('shows the window burst in order: two allowed payments inside one minute, then the refusal', () => {
    assert.equal(RED_TEAM.window[2]!.reason, 'WindowLimitExceeded');
    const times = RED_TEAM.window.map((step) => Date.parse(step.blockTime));
    assert.ok(times[0]! < times[1]! && times[1]! < times[2]!, 'in order');
    assert.ok(times[2]! - times[0]! < 60_000, 'all three inside one 60-second window');
    const event = redTeamFile.window.entries[2]!.decisionEvent as Record<string, unknown>;
    assert.equal(event.reason, 'WindowLimitExceeded');
  });

  it('records each runtime refusal as the raw RPC error, with no transaction, and none of them reached a block', () => {
    for (const refusal of redTeamFile.protocolRefusals) {
      assert.ok(!('tx' in refusal), 'no transaction hash');
      assert.ok(!('blockTime' in refusal), 'no block');
      assert.match(String(refusal.label), /Rejected by the NEAR runtime before any contract code ran; no block/);
      assert.ok(JSON.stringify(refusal.rawRpcResponse).includes(String(refusal.expectedError)));
      assert.ok(!('result' in (refusal.rawRpcResponse as object)), 'the node returned an error, not a result');
    }
    assert.deepEqual(RED_TEAM.refusals.map((r) => r.error), ['MethodNameMismatch', 'RequiresFullAccess', 'RequiresFullAccess', 'RequiresFullAccess', 'ReceiverMismatch']);
  });

  it('builds every link from the hash, and the links agree with the recorded ones', () => {
    for (const entry of [...redTeamFile.onChainDenials, ...redTeamFile.window.entries]) {
      assert.match(String(entry.tx), /^[1-9A-HJ-NP-Za-km-z]{43,44}$/);
      assert.equal(nearTxUrl(String(entry.tx)), entry.explorer);
    }
  });

  it('refuses a recorded run whose story does not hold together', () => {
    const broken = structuredClone(redTeamFile);
    (broken.onChainDenials[0]!.decisionEvent as Record<string, unknown>).reason = 'RecipientNotAllowed';
    assert.throws(() => parseRedTeam(broken), /decision event does not match/);
    const noReason = structuredClone(redTeamFile);
    delete noReason.onChainDenials[0]!.reason;
    assert.throws(() => parseRedTeam(noReason), /names no reason/);
    const wrongError = structuredClone(redTeamFile);
    wrongError.protocolRefusals[0]!.expectedError = 'SomethingElse';
    assert.throws(() => parseRedTeam(wrongError), /does not contain/);
  });
});

describe('the red-team log on the page', () => {
  const html = draw(<RedTeam />);
  const words = text(html);

  it('opens with what it is: real attacks, refused, most on chain', () => {
    assert.ok(words.includes('Every attack below was run against the live contract. It refused them all.'));
    assert.ok(words.includes("so you can open each failed transaction and read the contract's own reason in its log"));
    assert.ok(words.includes('these are real calls, signed with the agent\'s real key'));
    assert.ok(words.includes('9 were refused by the contract and 5 by the NEAR runtime'));
  });

  it('groups the denials by reason, each row with its attack, its code and its plain line', () => {
    for (const title of ['Over the cap', 'Not on the allowlist', 'An asset with no policy']) assert.ok(words.includes(title), title);
    for (const code of ['PerTxCapExceeded', 'RecipientNotAllowed', 'AssetNotAllowed']) assert.ok(html.includes(`<span class="stamp stamp--deny">${code}</span>`), code);
    assert.ok(words.includes('This one request is larger than the per-transaction cap.'));
    assert.ok(words.includes("The recipient is not on the owner's allowlist."));
    assert.ok(words.includes('This asset has no policy, so it cannot be moved.'));
    assert.ok(words.includes('on the token transfer leg'), 'a token payment says which leg refused it');
  });

  it('links every on-chain transaction to the explorer, once', () => {
    for (const attempt of [...RED_TEAM.denials, ...RED_TEAM.window]) {
      assert.equal(count(html, `href="${nearTxUrl(attempt.tx)}"`), 1, attempt.id);
    }
    const links = [...html.matchAll(/<a [^>]*href="https:\/\/testnet\.nearblocks\.io[^>]*>/g)].map((m) => m[0]);
    assert.equal(links.length, 11);
    for (const link of links) assert.match(link, /target="_blank"[^>]*rel="noopener noreferrer"|rel="noopener noreferrer"[^>]*target="_blank"|target="_blank"/);
  });

  it('makes the window burst a highlight: three linked requests in order, two allowed then one refused', () => {
    assert.ok(words.includes('The window: a limit one dry run cannot reach'));
    const section = html.slice(html.indexOf('rt-window'), html.indexOf('rt-refusals'));
    const stamps = [...section.matchAll(/<span class="stamp stamp--(allow|deny)">([^<]+)<\/span>/g)].map((m) => `${m[1]}:${m[2]}`);
    assert.deepEqual(stamps, ['allow:ALLOW', 'allow:ALLOW', 'deny:WindowLimitExceeded']);
    const hrefs = [...section.matchAll(/href="(https:\/\/testnet\.nearblocks\.io\/txns\/[^"]+)"/g)].map((m) => m[1]);
    assert.deepEqual(hrefs, RED_TEAM.window.map((step) => nearTxUrl(step.tx)));
    assert.ok(text(section).includes('Every request was within the per-transaction cap. The third was refused because together they would pass the window'));
  });

  it('labels the runtime refusals as never reaching a block, shows the raw errors, and links no transaction for them', () => {
    const section = html.slice(html.indexOf('rt-refusals'));
    const sectionText = text(section);
    assert.ok(sectionText.includes('Refused by the NEAR runtime, before any contract code ran'));
    assert.ok(sectionText.includes('These never reached a block, so there is no transaction to open'));
    for (const error of ['MethodNameMismatch', 'RequiresFullAccess', 'ReceiverMismatch']) assert.ok(section.includes(`stamp--deny">${error}<`), error);
    assert.equal(count(section, 'testnet.nearblocks.io'), 0, 'no explorer link for a call that never reached a block');
    assert.equal(count(section, '<details class="more"><summary>The raw RPC error</summary>'), 5);
    assert.ok(section.includes('&quot;MethodNameMismatch&quot;'), 'the raw error is on the page');
    assert.ok(sectionText.includes('No block, so no transaction on the explorer'));
    assert.ok(sectionText.includes('The key may call only its three request methods'));
  });

  it('keeps the raw errors folded away until asked for', () => {
    assert.ok(!/<details[^>]*\bopen\b/.test(html));
  });
});

describe('the recorded freeze drill', () => {
  it('goes freeze, refusal, proposal, execution, in that order, on chain time', () => {
    assert.deepEqual(FREEZE_DRILL.entries.map((e) => e.id), ['freeze', 'denied-while-frozen', 'propose-unfreeze', 'execute-unfreeze']);
    const times = FREEZE_DRILL.entries.map((e) => Date.parse(e.blockTime));
    for (let i = 1; i < times.length; i += 1) assert.ok(times[i]! > times[i - 1]!, FREEZE_DRILL.entries[i]!.id);
  });

  it('refused the agent with Frozen, on chain, and the contract\'s own event says so', () => {
    assert.equal(FREEZE_DRILL.entries[1]!.reason, 'Frozen');
    const event = drillFile.entries[1]!.decisionEvent as Record<string, unknown>;
    assert.equal(event.decision, 'deny');
    assert.equal(event.reason, 'Frozen');
  });

  it('made the owner wait out the full delay: the execution is after the queued unfreeze became ready', () => {
    assert.equal(FREEZE_DRILL.delaySeconds, 120);
    const propose = Date.parse(FREEZE_DRILL.entries[2]!.blockTime);
    const execute = Date.parse(FREEZE_DRILL.entries[3]!.blockTime);
    assert.ok(execute >= Date.parse(FREEZE_DRILL.readyAt), 'executed only once ready');
    assert.ok(execute - propose >= 120_000 - 1000, 'at least the delay passed between the proposal and the execution');
  });

  it('ends with the contract Active again', () => {
    assert.equal(FREEZE_DRILL.finalStatus, 'Active');
  });

  it('refuses a drill that is out of order, or that did not wait', () => {
    const swapped = structuredClone(drillFile);
    [swapped.entries[0], swapped.entries[1]] = [swapped.entries[1]!, swapped.entries[0]!];
    assert.throws(() => parseFreezeDrill(swapped), /steps are not/);
  });
});

describe('the freeze drill on the page', () => {
  const html = draw(<FreezeDrill />);
  const words = text(html);

  it('says stopping is instant and resuming is deliberately delayed, even for the owner', () => {
    assert.ok(words.includes('Stopping is instant. Resuming is deliberately delayed.'));
    assert.ok(words.includes('The guardian can halt everything with one call.'));
    assert.ok(words.includes('even the owner must wait out the 120-second timelock to resume'));
  });

  it('shows the four steps in order, each with its block time and an explorer link', () => {
    const order = ['The guardian', 'The agent', 'The owner'].map((who) => words.indexOf(who));
    assert.ok(order[0]! < order[1]! && order[1]! < order[2]!);
    for (const entry of FREEZE_DRILL.entries) {
      assert.equal(count(html, `href="${nearTxUrl(entry.tx)}"`), 1, entry.id);
      assert.ok(words.includes(`${utcClock(entry.blockTime)} UTC`), entry.id);
    }
    const hrefs = [...html.matchAll(/href="(https:\/\/testnet\.nearblocks\.io\/txns\/[^"]+)"/g)].map((m) => m[1]);
    assert.deepEqual(hrefs, FREEZE_DRILL.entries.map((e) => nearTxUrl(e.tx)));
  });

  it('shows the refusal as Frozen, the unfreeze as queued rather than applied, and the end as Active', () => {
    assert.match(html, /<span class="stamp stamp--deny">Frozen<\/span>/);
    assert.match(words, /The contract is Frozen ?, at once\./);
    assert.ok(words.includes('Queued, not applied: it is a loosening.'));
    assert.match(html, /<span class="is-verified">Active<\/span>/);
  });

  it('states the measured waits, from the blocks themselves', () => {
    const [freeze, refused, propose, execute] = FREEZE_DRILL.entries;
    assert.ok(words.includes(`From the freeze to the refused request: ${elapsed(freeze!.blockTime, refused!.blockTime)}`));
    assert.ok(words.includes(`From the proposal to the execution: ${elapsed(propose!.blockTime, execute!.blockTime)}, against a 120-second delay`));
    assert.equal(elapsed('2026-11-14T16:07:19.000Z', '2026-11-14T16:09:34.000Z'), '2 min 15 s');
    assert.equal(elapsed('2026-11-14T16:00:00.000Z', '2026-11-14T16:00:10.000Z'), '10 s');
  });
});

describe('the story on /app', () => {
  const html = renderToString(
    <StaticRouter location="/app">
      <App />
    </StaticRouter>,
  );

  it('runs: the live console, the red-team log, the real run, then the freeze drill as the finale', () => {
    const marks = ['01 / Agent side', '02 / Money side', '03 / The red-team log', '04 / The real run', '05 / Containment and recovery'].map((mark) => html.indexOf(mark));
    for (const [i, at] of marks.entries()) assert.ok(at > -1, `section ${i + 1} is present`);
    for (let i = 1; i < marks.length; i += 1) assert.ok(marks[i]! > marks[i - 1]!, `section ${i + 1} follows section ${i}`);
  });

  it('holds no key, and says in the evidence that no key is in the files', () => {
    for (const file of ['red-team.json', 'freeze-drill.json']) {
      const raw = readFileSync(new URL(`./data/${file}`, import.meta.url), 'utf8');
      assert.ok(!/private[_ ]?key|secret_key|seed[_ ]?phrase|mnemonic/i.test(raw.replace(/its private half was discarded/g, '')), file);
      assert.ok(!/ed25519:[1-9A-HJ-NP-Za-km-z]{80,}|secp256k1:[1-9A-HJ-NP-Za-km-z]{40,48}(?![1-9A-HJ-NP-Za-km-z])/.test(raw), `${file}: no private key shape`);
    }
  });
});
