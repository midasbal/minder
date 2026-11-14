import assert from 'node:assert/strict';
import { existsSync, lstatSync, readFileSync, statSync } from 'node:fs';
import { describe, it } from 'node:test';
import { fileURLToPath } from 'node:url';

import { FREEZE_DRILL, RED_TEAM } from './chain/evidence';
import { LIVE_RUN } from './chain/liverun';
import { AUDIT_URL, CONTRACT_ID, DESIGN_URL, EXPLORER_URL, REPO_URL, VERIFY_SCRIPT_URL, WASM_SHA256 } from './links';

/*
  llms.txt is an index for AI readers. These tests hold it to two things: every fact in it matches the artifact it
  comes from, and it stays a neutral index that describes and does not instruct, persuade or address its reader.
*/

const repo = (path: string) => fileURLToPath(new URL(`../../../${path}`, import.meta.url));
const source = readFileSync(repo('web/client/public/llms.txt'), 'utf8');
const lines = source.split('\n');
const readme = readFileSync(repo('README.md'), 'utf8');
const audit = readFileSync(repo('AUDIT.md'), 'utf8');
const design = readFileSync(repo('contract/DESIGN.md'), 'utf8');
const cargo = readFileSync(repo('contract/Cargo.toml'), 'utf8');
const contractSource = readFileSync(repo('contract/src/lib.rs'), 'utf8');

describe('llms.txt follows the convention', () => {
  it('opens with an H1 of the project name, then a one-line blockquote summary', () => {
    assert.equal(lines[0], '# Minder');
    assert.equal(lines[1], '');
    assert.ok(lines[2]!.startsWith('> '), 'a blockquote summary');
    assert.ok(!lines[2]!.slice(2).includes('\n') && lines[3] === '', 'on one line');
  });

  it('has short sections, and describes every link in a line', () => {
    const sections = lines.filter((line) => line.startsWith('## ')).map((line) => line.slice(3));
    assert.deepEqual(sections, ['What it is', 'Custody: keyless', 'Verification and its bounds', 'Known limitations', 'Live deployment', 'Repository', 'Recorded evidence', 'Optional']);
    const links = lines.filter((line) => /\]\(https?:/.test(line) && line.startsWith('- ['));
    assert.ok(links.length >= 12);
    for (const line of links.filter((l) => !l.includes('Twitter/X'))) assert.match(line, /^- \[[^\]]+\]\(https?:\/\/[^)]+\): \S.+/, line);
  });

  it('is plain text a machine can read: ASCII punctuation, no HTML, no images', () => {
    assert.ok(!/[^\x00-\x7f]/.test(source), 'ASCII only');
    assert.ok(!/<[a-z!/]/i.test(source) && !source.includes('!['));
  });
});

describe('every link goes where it says', () => {
  const urls = [...source.matchAll(/\]\((https?:\/\/[^)]+)\)/g)].map((match) => match[1]!);

  it('points the repository links at files and folders that exist', () => {
    const repoLinks = urls.filter((url) => url.startsWith(REPO_URL + '/'));
    assert.ok(repoLinks.length >= 9);
    for (const url of repoLinks) {
      const path = url.replace(/^.*\/(?:blob|tree)\/main\//, '');
      assert.ok(existsSync(repo(path)), `${url} -> ${path}`);
      assert.equal(url.includes('/tree/') ? statSync(repo(path)).isDirectory() : statSync(repo(path)).isFile(), true, path);
    }
  });

  it('uses the same live site, explorer, audit, design and script links as the site and the README', () => {
    const site = /https:\/\/minder-near\.up\.railway\.app/.exec(readme)![0];
    assert.ok(urls.includes(`${site}/app`) && urls.includes(`${site}/docs`));
    assert.ok(urls.includes(EXPLORER_URL) && urls.includes(AUDIT_URL) && urls.includes(DESIGN_URL) && urls.includes(VERIFY_SCRIPT_URL));
    assert.ok(urls.includes('https://x.com/wjmdiary'));
    assert.equal(new Set(urls).size, urls.length, 'no link listed twice');
  });
});

describe('every fact matches its source', () => {
  it('names the deployed contract, the deployed code hash and the EVM address from the project\'s own records', () => {
    assert.ok(source.includes(`\`${CONTRACT_ID}\``));
    assert.ok(source.includes(WASM_SHA256));
    assert.ok(source.includes(LIVE_RUN.recordedProvenance.derivedAddress));
    assert.ok(readme.includes(LIVE_RUN.recordedProvenance.derivedAddress));
  });

  it('gives the agent\'s three methods and its allowance as the contract has them', () => {
    const methods = /const AGENT_METHODS: &str = "([^"]+)";/.exec(contractSource)![1]!.split(',');
    assert.equal(methods.length, 3);
    for (const method of methods) assert.ok(source.includes(`\`${method}\``), method);
    assert.ok(/const AGENT_KEY_ALLOWANCE: NearToken = NearToken::from_millinear\(250\);/.test(contractSource), 'the contract\'s allowance is 0.250 NEAR');
    assert.ok(source.includes('0.25 NEAR') && design.includes('0.25 NEAR'));
    assert.match(cargo, /near-sdk = \{ version = "=5\.29\.1"/);
    assert.ok(source.includes('near-sdk 5.29.1'));
  });

  it('quotes the proof, mutation and corpus figures as AUDIT.md, DESIGN.md and the README give them', () => {
    const facts: Array<[string, string, string]> = [
      ['Kani 0.68.0', audit, 'Kani 0.68.0'],
      ['43 proof harnesses', audit, 'Kani: 43 harnesses'],
      ['cargo-mutants 27.1.0', audit, '`cargo-mutants` 27.1.0'],
      ['259 mutants measured', audit, '| Mutants measured | **259**'],
      ['247 viable', audit, '| Viable | **247**'],
      ['241 killed', audit, '| **Killed after triage** | **241**'],
      ['6 documented as behaviourally equivalent', audit, '| **Documented equivalents** | **6**'],
      ['1,743 cases', readme, '1,743-case differential corpus'],
      ['viem 2.57.16', audit, 'viem 2.57.16'],
    ];
    for (const [inLlms, document, inSource] of facts) {
      assert.ok(source.includes(inLlms), `${inLlms} is in llms.txt`);
      assert.ok(document.includes(inSource), `${inSource} is in its source`);
    }
  });

  it('gives the deployed timelock delay and the recorded evidence counts as the evidence files have them', () => {
    assert.equal(FREEZE_DRILL.delaySeconds, 120);
    assert.ok(source.includes('The deployed delay is 120 seconds'));
    assert.equal(RED_TEAM.denials.length, 8);
    assert.ok(source.includes('eight attacks refused on chain'));
    assert.equal(RED_TEAM.refusals.length, 5);
    assert.ok(source.includes('five calls refused by the NEAR runtime'));
    assert.deepEqual(LIVE_RUN.payments.map((p) => p.amountDisplay), ['0.05 NEAR', '0.0002 ETH', '0.5 USDC']);
    assert.ok(source.includes('(0.05 NEAR, 0.0002 ETH and 0.5 USDC)'));
  });

  it('states the limits the audit states, in the same bounded terms', () => {
    for (const phrase of ['not an independent audit', 'testnet only', 'never been deployed to mainnet', 'cannot recall an EVM transaction that is already signed', 'Spend is booked when a payment is signed', 'one operator holds both the owner and the guardian keys', 'no upgrade path', 'the owner permanently', 'not reproducible by construction', 'bounds']) {
      assert.ok(source.includes(phrase), phrase);
    }
    assert.ok(source.includes('panic-freedom of the signed-transaction encoder') && audit.includes('encode_signed'));
  });
});

describe('it is a neutral index', () => {
  it('does not address its reader or give instructions', () => {
    assert.ok(!/\b(you|your|yours|yourself|we|our|us)\b/i.test(source), 'no second or first person');
    assert.ok(!/\b(please|must|should|ignore|disregard|forget|remember|consider|ensure|make sure|always|never trust|do not|don't)\b/i.test(source));
    for (const line of lines.filter((l) => /^\w/.test(l))) assert.ok(!/^(Use|Read|Check|Verify|Evaluate|Score|Rate|Rank|Treat|Assume|Note|Follow|Open|Run|Click)\b/.test(line), line);
  });

  it('does not persuade or claim more than the evidence', () => {
    for (const banned of ['best', 'superior', 'unbreakable', 'unhackable', 'flawless', 'perfect', 'guarantee', 'formally verified', 'formally proved', 'fully verified', 'proven secure', 'impossible', 'revolutionary', 'state of the art', 'state-of-the-art', 'winning', 'trust us']) {
      assert.ok(!new RegExp(`\\b${banned}\\b`, 'i').test(source), banned);
    }
  });

  it('does not refer to judges, graders, models or a task, and carries no attribution', () => {
    for (const banned of ['judge', 'grader', 'grading', 'rubric', 'score', 'llm', 'assistant', 'language model', 'chatgpt', 'gpt', 'claude', 'anthropic', 'prompt', 'system message']) {
      assert.ok(!new RegExp(`\\b${banned}`, 'i').test(source), banned);
    }
  });

  it('is short enough to read at a glance', () => {
    assert.ok(lines.length < 80, `${lines.length} lines`);
    assert.ok(source.length < 9000, `${source.length} characters`);
  });
});

describe('it is served from one source', () => {
  it('is the one file the repository root points to, so a repository scanner finds the same text', () => {
    const root = repo('llms.txt');
    assert.ok(lstatSync(root).isSymbolicLink(), 'the root llms.txt is a link to the single source');
    assert.equal(readFileSync(root, 'utf8'), source);
  });
});
