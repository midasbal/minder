import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { renderToString } from 'react-dom/server';
import { StaticRouter } from 'react-router';

import { App } from './App';
import { ALL_IDS, TOC } from './docs/toc';
import { EXPLORER_URL, REPO_URL } from './links';

/** The server-rendered HTML of the whole app at a URL: the chrome and the page. */
function render(url: string): string {
  return renderToString(
    <StaticRouter location={url}>
      <App />
    </StaticRouter>,
  );
}

const ROUTES = ['/', '/app', '/docs', '/nothing-here'];

/** The visible text of rendered HTML, with typographic quotes and entities normalised to plain ones. */
function visibleText(html: string): string {
  return html
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

  it('keeps the hero as it was', () => {
    assert.ok(html.includes('A policy firewall between an AI agent and money.'));
    assert.match(html, /<em[^>]*class="headline__line headline__line--accent"[^>]*>Assume the agent is already compromised\.<\/em>/);
  });

  it('has the three calls to action', () => {
    assert.match(html, /<a[^>]*href="\/app"[^>]*>Try the live firewall<\/a>/);
    assert.match(html, /<a[^>]*href="\/docs"[^>]*>Read the docs<\/a>/);
    assert.match(html, /<a[^>]*href="https:\/\/github.com\/midasbal\/minder"[^>]*>View source/);
  });

  it('has every chapter, in order, and the closing band', () => {
    const titles = [
      'The agent will be compromised. Plan for it.',
      'One narrow key. One deterministic gate.',
      'The contract has no master key.',
      'We prove the limits, not just test them.',
      'It gates the signature itself.',
      'See it refuse.',
    ];
    let from = 0;
    for (const title of titles) {
      const at = html.indexOf(title, from);
      assert.ok(at >= 0, title);
      from = at;
    }
    assert.ok(html.includes('Drive the compromised agent yourself and watch the firewall hold.'));
  });

  it('carries the chapter copy verbatim', () => {
    const text = visibleText(html);
    for (const sentence of [
      'AI agents are getting wallets. Prompt injection, a poisoned tool, a jailbreak: any of them can turn the agent against its owner. Minder does not try to keep the agent clean. It assumes the agent is already compromised, contains what it can do, and proves the containment.',
      'The agent holds a single function-call key: narrow, revocable, time-boxed, scoped to three payment methods and nothing else. It never touches the funds. The contract is the only path to money, and every request, local or cross-chain, meets the same deterministic policy before a cent moves.',
      'Through NEAR Chain Signatures, the contract is the sole signer for its own EVM address. A cross-chain payment is not approved off-chain and relayed; the firewall decides whether the transaction gets signed at all. Real signatures, live on Sepolia.',
      "Minder's account is deployed with zero full-access keys. There is no master key on the account to phish, leak, or misuse. Governance lives in a separate owner account and is bound by the contract itself: it can tighten or freeze instantly, but every policy loosening must clear a public timelock. This is structural, not a setting, and the keyless account is verifiable live on chain.",
      'The limit logic is not "trust us." Its core is formally verified with Kani\'s bounded model checking, hardened against 200+ code mutations, and cross-checked against a differential corpus of EVM encodings. The guarantees hold within stated bounds, not merely across the cases a test suite happened to try.',
    ]) {
      assert.ok(text.includes(sentence), sentence.slice(0, 60));
    }
  });

  it('no longer makes the claims that were withdrawn', () => {
    const text = visibleText(html);
    for (const withdrawn of [
      'Containment is a theorem',
      'a proof, not a promise',
      'No owner key sits on the account',
      'the only authority is the policy in the code',
      'formally proved with Kani',
    ]) {
      assert.ok(!text.includes(withdrawn), withdrawn);
    }
  });

  it('shows the static keyless badge and says it is static', () => {
    assert.match(html, /class="badge__count"[^>]*>0</);
    assert.ok(html.includes('full-access keys'));
    assert.ok(html.includes('Static for now'));
  });

  it('shows the figures from the design document', () => {
    for (const figure of ['43', '241', '/247', '1,743']) assert.ok(html.includes(figure), figure);
  });

  it('points to the exact bounds of the proofs', () => {
    assert.match(html, /<a[^>]*href="\/docs#proofs"/);
  });

  it('does not carry the reserved app slots', () => {
    assert.ok(!html.includes('Agent control surface'));
    assert.ok(!html.includes('Status panel'));
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
