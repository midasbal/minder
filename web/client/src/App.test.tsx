import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { describe, it } from 'node:test';

import { renderToString } from 'react-dom/server';
import { StaticRouter } from 'react-router';

import { App } from './App';
import { LIVE_RUN, nearTxUrl, sepoliaTxUrl } from './chain/liverun';
import { ALL_IDS, TOC } from './docs/toc';
import { FAQ } from './faq';
import { AUDIT_URL, CONTRACT_ID, EXPLORER_URL, README_URL, REPO_URL, VERIFY_SCRIPT_URL } from './links';

/** The server-rendered HTML of the whole app at a URL: the chrome and the page. */
function render(url: string): string {
  return renderToString(
    <StaticRouter location={url}>
      <App />
    </StaticRouter>,
  );
}

const ROUTES = ['/', '/app', '/docs', '/nothing-here'];

/**
 * The visible text of rendered HTML: inline tags vanish, block tags separate words, and typographic quotes
 * and entities are normalised to plain ones.
 */
function visibleText(html: string): string {
  return html
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/<\/?(strong|em|code|a|span)\b[^>]*>/g, '')
    .replace(/<[^>]+>/g, ' ')
    .replace(/&#x27;|\u2019/g, "'")
    .replace(/&quot;|\u201c|\u201d/g, '"')
    .replace(/&amp;/g, '&')
    .replace(/\s+/g, ' ');
}

describe('the persistent chrome', () => {
  for (const url of ROUTES) {
    it(`is on ${url}: header, nav, testnet chip, footer, relay chip`, () => {
      const html = render(url);
      assert.match(html, /<header class="site-header"/);
      assert.match(html, /<a class="wordmark"[^>]*href="\/"/);
      for (const [href, label] of [
        ['/', 'Overview'],
        ['/app', 'Live demo'],
        ['/docs', 'Docs'],
      ] as const) {
        assert.match(html, new RegExp(`<a[^>]*href="${href}"[^>]*>${label}</a>`), `${label} link`);
      }
      assert.ok(html.includes(`href="${REPO_URL}"`), 'the Source link');
      assert.ok(html.includes('Testnet only'));
      assert.match(html, /<footer class="site-footer"/);
      assert.ok(html.includes(`href="${EXPLORER_URL}"`), 'the contract on the explorer');
      assert.ok(html.includes('NEAR testnet'));
      assert.match(html, /relay: checking/);
    });
  }

  it('opens every external link in a new tab without leaking the opener', () => {
    for (const url of ROUTES) {
      const anchors = render(url).match(/<a [^>]*target="_blank"[^>]*>/g) ?? [];
      assert.ok(anchors.length > 0, url);
      for (const anchor of anchors) assert.match(anchor, /rel="noopener noreferrer"/, anchor);
    }
  });
});

describe('the landing page', () => {
  const html = render('/');
  const text = visibleText(html);

  it('opens with the headline, the subhead and the two calls to action', () => {
    assert.match(html, /<h1 class="hero__title">A policy firewall between an AI agent and money\.<\/h1>/);
    assert.ok(
      text.includes(
        'Give an autonomous agent a wallet and the question is no longer if it gets compromised, but what it can do when it does. Minder assumes the agent is already compromised, contains what it can spend, and proves the containment.',
      ),
    );
    // One phrase is emphasised, in the ink colour; there is no coloured single word.
    assert.deepEqual(html.match(/<strong>[^<]*<\/strong>/g), [
      '<strong>Minder assumes the agent is already compromised</strong>',
    ]);
    assert.match(html, /<a class="btn btn--primary"[^>]*href="\/app"[^>]*>Open the live firewall<\/a>/);
    assert.match(html, /<a class="btn btn--secondary"[^>]*href="\/docs"[^>]*>Read the docs<\/a>/);
  });

  it('has the sections in order, each with its label and heading, and the closing band', () => {
    const parts = [
      '01 · THREAT MODEL',
      'The agent will be compromised. Plan for it.',
      '02 · WHAT HOLDS, AND WHY YOU CAN CHECK IT',
      "Two guarantees that don't rest on trusting us.",
      '03 · CROSS-CHAIN',
      'It gates the signature itself.',
      'See it refuse.',
    ];
    let from = 0;
    for (const part of parts) {
      const at = text.indexOf(part, from);
      assert.ok(at >= 0, part);
      from = at;
    }
  });

  it('carries the section copy verbatim', () => {
    for (const sentence of [
      "Prompt injection, a poisoned tool, a jailbreak: any of them can turn an agent against its owner. Minder does not try to keep the agent clean. It holds the agent at arm's length behind a single narrow key, routes every movement of money through one deterministic gate, and makes the contract the only path to funds. Containment, not trust.",
      'Through NEAR Chain Signatures the contract is the sole signer for its own EVM address. A cross-chain payment is never approved off-chain and relayed: the firewall decides whether the transaction gets signed at all. The output is a real EIP-1559 transaction, broadcast to Sepolia.',
      'Drive the compromised agent yourself and watch the firewall hold.',
    ]) {
      assert.ok(text.includes(sentence), sentence.slice(0, 60));
    }
  });

  it('shows the policy gate: the contract, the policy, and three static decisions with their stamps', () => {
    for (const part of [
      'POLICY GATE',
      CONTRACT_ID,
      'AGENT · untrusted',
      'fn-call key',
      '3 methods · time-boxed · revocable',
      'FUNDS',
      'treasury',
      'NEAR + EVM via chain signatures',
      'Illustrative decisions on the deployed policy. Static.',
    ]) {
      assert.ok(text.includes(part), part);
    }
    for (const [key, value] of [
      ['per-tx', '0.50'],
      ['window', '1.00 / 60s'],
      ['budget', '2.00'],
      ['allowlist', '1'],
    ]) {
      assert.ok(html.includes(`<dt>${key}</dt><dd>${value}</dd>`), key);
    }
    for (const [request, reason, stamp, kind] of [
      ['transfer 0.40 → vendor.near', 'within every limit', 'ALLOW', 'allow'],
      ['transfer 0.60 → vendor.near', 'exceeds per-tx cap 0.50', 'DENY', 'deny'],
      ['transfer 0.30 → unknown.near', 'recipient not on allowlist', 'DENY', 'deny'],
    ]) {
      const row = `<p class="decision__request">${request}</p><p class="decision__reason">${reason}</p></div><span class="stamp stamp--${kind}">${stamp}</span>`;
      assert.ok(html.includes(row), request);
    }
  });

  it('shows the two datasheets with their rows, tags and footnotes', () => {
    for (const row of [
      '<dt>full-access keys on account</dt><dd class="is-verified">0</dd>',
      '<dt>agent key authority</dt><dd>3 methods only</dd>',
      '<dt>governance</dt><dd>external owner</dd>',
      '<dt>policy loosening</dt><dd>timelocked</dd>',
      '<dt>kani proofs</dt><dd>43 · bounded</dd>',
      '<dt>mutations caught</dt><dd>200+</dd>',
      '<dt>differential corpus</dt><dd>1,743 EVM encodings</dd>',
      '<dt>keyless deploy</dt><dd class="is-verified">verified</dd>',
    ]) {
      assert.ok(html.includes(row), row);
    }
    for (const part of ['CUSTODY', 'keyless', 'VERIFICATION', 'provable']) assert.ok(text.includes(part), part);
    assert.ok(text.includes('Static values. Verify live: view_access_key_list + view_code hash'));
    assert.match(html, /<a[^>]*href="\/docs#proofs"[^>]*>Guarantees hold within stated bounds<\/a>/);
  });

  it('does not carry the old app placeholders, and does not scroll-reveal its sections', () => {
    assert.ok(!html.includes('Agent control surface'));
    assert.ok(!html.includes('Status panel'));
    const source = readFileSync(new URL('./pages/Landing.tsx', import.meta.url), 'utf8');
    assert.ok(!source.includes('whileInView'));
  });

  it('no longer makes the claims that were withdrawn', () => {
    for (const withdrawn of [
      'theorem',
      'a proof, not a promise',
      'No owner key sits on the account',
      'the only authority is the policy in the code',
      'formally proved with Kani',
    ]) {
      assert.ok(!text.includes(withdrawn), withdrawn);
    }
  });
});

describe('the sensing surface behind every page', () => {
  it('is a canvas on every route, hidden from assistive technology', () => {
    for (const url of ROUTES) {
      assert.match(render(url), /<canvas class="field" aria-hidden="true"><\/canvas>/, url);
    }
  });
});

describe('the app shell', () => {
  const html = render('/app');

  it('holds the agent console, the contract panel and the boundary', () => {
    const text = visibleText(html);
    assert.ok(text.includes('Ask the contract'));
    assert.ok(text.includes('Attack presets'));
    assert.ok(text.includes('Reading the contract from the chain'));
    assert.ok(text.includes('Policy boundary'));
  });

  it('is no longer a placeholder, and shows nothing read from the chain on the server', () => {
    const text = visibleText(html);
    assert.ok(!text.includes('placeholder'));
    assert.ok(!text.includes('Read from the chain at'), 'no reading is claimed before one is made');
    assert.ok(!text.includes('ALLOW') && !text.includes('DENY'), 'no verdict is shown before a request');
  });
});

describe('the docs', () => {
  const html = render('/docs');

  it('has a heading for every entry in the contents', () => {
    for (const id of ALL_IDS) assert.match(html, new RegExp(`<h[23][^>]*id="${id}"`), id);
  });

  it('links every entry in the contents', () => {
    for (const entry of TOC) assert.ok(html.includes(`href="/docs#${entry.id}"`), entry.id);
  });

  it('names the three agent methods and the non-guarantees', () => {
    for (const method of ['request_local_transfer', 'request_evm_payment', 'request_evm_token_payment']) {
      assert.ok(html.includes(method), method);
    }
    assert.ok(html.includes('A freeze does not recall a transaction already signed'));
  });

  it('shows the Kani count as its two parts, so it can be traced to the design document', () => {
    const text = visibleText(html);
    assert.ok(text.includes('18 proofs of the encoder, the sliding window and the firewall decision'));
    assert.ok(text.includes('25 of the timelock classifier: 18 + 25 = 43'));
  });

  it('keeps the exact mutation figures, with their caveat, that the landing page rounds', () => {
    const text = visibleText(html);
    assert.ok(text.includes('241 of the 247 viable mutants are killed by tests'));
    assert.ok(text.includes('that new scope has not been re-run through the mutation tool'));
  });

  it('says what the live demo does today, and what is not wired yet', () => {
    const words = visibleText(html);
    assert.ok(words.includes('live for everything that needs no key'));
    assert.ok(words.includes('are not wired yet'));
    assert.ok(!words.includes('is a placeholder'));
  });
});

const QUESTIONS = [
  "Can't the agent just be reprogrammed or jailbroken to bypass the firewall?",
  "What if the agent's key is stolen?",
  'Is this really deployed, or a mockup?',
  'What does "keyless" mean, and what is the catch?',
  'How can a NEAR contract move money on Ethereum without holding an EVM key?',
  'What if the owner is malicious?',
  'Can a payment be undone once it is signed?',
  'Is "provable" real formal verification?',
  'Can I use this for my own agent?',
  'Why NEAR?',
];

const LANDING_QUESTIONS = [QUESTIONS[0], QUESTIONS[1], QUESTIONS[2], QUESTIONS[4], QUESTIONS[7], QUESTIONS[8]];

/** The question rows of a page, in order, as plain text. */
function questionsOf(html: string): string[] {
  return [...html.matchAll(/<summary class="faq__q">([\s\S]*?)<\/summary>/g)].map((match) => visibleText(match[1]!).trim());
}

describe('the FAQ', () => {
  const docs = render('/docs');
  const landing = render('/');

  it('answers all ten questions in the docs, each an expandable row with an answer', () => {
    assert.deepEqual(questionsOf(docs), QUESTIONS);
    assert.equal((docs.match(/<details class="faq__item"/g) ?? []).length, 10);
    assert.equal(FAQ.length, 10);
    for (const item of FAQ) assert.ok(visibleText(docs).length > 0 && docs.includes(`id="${item.id}"`), item.id);
  });

  it('is its own section in the contents, before the reference', () => {
    const ids = TOC.map((entry) => entry.id);
    assert.ok(ids.indexOf('faq') > ids.indexOf('live-demo') && ids.indexOf('faq') < ids.indexOf('reference'));
    assert.match(docs, /<h2 id="faq" class="doc-h2"><span class="doc-h2__number">6<\/span>Frequently asked questions<\/h2>/);
    assert.match(docs, /<h2 id="reference" class="doc-h2"><span class="doc-h2__number">7<\/span>Reference<\/h2>/);
  });

  it('shows the landing page the six most useful questions, between the cross-chain section and the closing band', () => {
    assert.deepEqual(questionsOf(landing), LANDING_QUESTIONS);
    const text = visibleText(landing);
    assert.ok(text.indexOf('It gates the signature itself.') < text.indexOf('04 · QUESTIONS'));
    assert.ok(text.indexOf('The questions you are about to ask.') < text.indexOf('See it refuse.'));
    assert.match(landing, /<a[^>]*href="\/docs#faq"[^>]*>docs FAQ<\/a>/);
    for (const left of [QUESTIONS[3], QUESTIONS[5], QUESTIONS[6], QUESTIONS[9]]) assert.ok(!questionsOf(landing).includes(left), left);
  });

  it('states what holds and what does not, with the figures the rest of the site uses', () => {
    const text = visibleText(docs);
    for (const claim of [
      'the firewall is not in the agent',
      'cannot transfer, deploy or add or delete keys',
      "fixed 0.25 NEAR gas allowance",
      'it has never been deployed to mainnet',
      'there is no upgrade path',
      'the account that signs the deployment is the owner permanently',
      'a hostile owner and guardian acting together can still drain after the delay',
      'one operator holds both keys with a 120-second delay',
      'This is the documented boundary called G2',
      'machine-checked within stated bounds, not complete formal verification',
      '43 harnesses',
      '241 of 247 viable mutants killed, the other 6 documented as equivalent',
      'a 1,743-case differential corpus',
      'one mandate is active per contract',
      'there is no packaged client library',
      'this page does not claim it could not be built elsewhere',
    ]) {
      assert.ok(text.includes(claim), claim);
    }
  });

  it('does not claim more than it can: no absolutes about proofs, no promise to recall a signed payment', () => {
    const text = visibleText(docs + landing);
    for (const banned of ['formally proved', 'formally verified', 'fully verified', 'guaranteed safe', 'cannot be hacked', 'unhackable']) {
      assert.ok(!text.toLowerCase().includes(banned.toLowerCase()), banned);
    }
  });

  it('links to the evidence it cites', () => {
    assert.ok(docs.includes(`href="${VERIFY_SCRIPT_URL}"`));
    assert.ok(docs.includes(`href="${AUDIT_URL}"`));
    assert.ok(docs.includes(`href="${README_URL}"`));
    assert.ok(docs.includes(`href="${EXPLORER_URL}"`));
    assert.match(docs, /<a[^>]*href="\/app"[^>]*>console<\/a>/);
  });
});

describe('the docs, against what exists today', () => {
  const html = render('/docs');
  const text = visibleText(html);

  it('points every internal link at a heading that exists', () => {
    const targets = new Set([...html.matchAll(/href="\/docs#([a-z-]+)"/g)].map((match) => match[1]!));
    assert.ok(targets.size > 8);
    for (const target of targets) assert.ok(ALL_IDS.includes(target), `/docs#${target} has no heading`);
  });

  it('refers to the artifacts that now exist: the console, the self-audit, the verification script, the README', () => {
    for (const link of [AUDIT_URL, VERIFY_SCRIPT_URL, README_URL]) assert.ok(html.includes(`href="${link}"`), link);
    assert.match(html, /<a[^>]*href="\/app"[^>]*>The live console<\/a>/);
    assert.ok(text.includes('self-audit, not an independent one'));
    assert.ok(text.includes('builds the contract from a clean checkout'));
  });

  it('lists the recorded real run with a working explorer link for every hash', () => {
    for (const payment of LIVE_RUN.payments) {
      assert.ok(html.includes(`href="${nearTxUrl(payment.nearTxHash)}"`), payment.nearTxHash);
      if (payment.sepoliaTxHash) assert.ok(html.includes(`href="${sepoliaTxUrl(payment.sepoliaTxHash)}"`), payment.sepoliaTxHash);
    }
    assert.ok(text.includes('The recorded real run'));
  });

  it('says what the proofs are, without calling them complete', () => {
    assert.ok(text.includes('None of this is a claim of complete formal verification'));
    assert.ok(text.includes('machine-checked with Kani bounded model checking'));
    assert.ok(text.includes('which is machine-checked with Kani within stated bounds'));
    assert.ok(!text.includes('formally proved'));
  });

  it('states the non-guarantees that came out of the live run and the audit', () => {
    for (const claim of [
      'Spend is booked when a payment is signed, not when it settles',
      'One mandate is active per contract',
      'In the demo deployment one operator holds both the owner and the guardian keys',
      'It is testnet only and not independently audited',
    ]) {
      assert.ok(text.includes(claim), claim);
    }
  });

  it('names the NEAR runtime as trusted to enforce the key, and the signer as relied on for availability', () => {
    assert.ok(text.includes('The NEAR runtime'));
    assert.ok(text.includes('Trusted to enforce access keys'));
    assert.ok(text.includes('It does rely on the signer being available'));
  });

  it('says today\'s public site holds no key, and does not promise one', () => {
    assert.ok(text.includes('Today the public site holds no key at all'));
    assert.ok(!text.includes('will hold the agent key only'));
    assert.ok(text.includes('the owner key is never held by a public service'));
  });

  it('says which scenarios the console covers and which it does not', () => {
    assert.ok(text.includes('Where to see it'));
    assert.ok(text.includes('Not reachable in one dry run'));
    assert.ok(text.includes('Not wired: it is an operator action'));
  });
});

describe('an unknown route', () => {
  it('renders the not-found page inside the chrome', () => {
    const html = render('/nothing-here');
    assert.ok(html.includes('Nothing here.'));
    assert.match(html, /<header class="site-header"/);
  });
});
