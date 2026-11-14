import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { describe, it } from 'node:test';

import { renderToString } from 'react-dom/server';
import { StaticRouter } from 'react-router';

import { App } from './App';
import { ALL_IDS, TOC } from './docs/toc';
import { CONTRACT_ID, EXPLORER_URL, REPO_URL } from './links';

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

  it('does not carry the reserved app slots, and does not scroll-reveal its sections', () => {
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

  it('holds the reserved slots and the boundary', () => {
    assert.ok(html.includes('Agent control surface'));
    assert.ok(html.includes('Status panel'));
    assert.ok(html.includes('Policy boundary'));
  });

  it('is a placeholder and says so', () => {
    assert.ok(html.includes('placeholder'));
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

  it('says the live demo is not wired yet', () => {
    assert.ok(html.includes('none of it is wired yet'));
  });
});

describe('an unknown route', () => {
  it('renders the not-found page inside the chrome', () => {
    const html = render('/nothing-here');
    assert.ok(html.includes('Nothing here.'));
    assert.match(html, /<header class="site-header"/);
  });
});
