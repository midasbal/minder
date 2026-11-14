import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { describe, it } from 'node:test';

/*
  The colour rules of the theme, enforced against the stylesheets themselves:
  the tokens, contrast, and where each colour is allowed to appear.
*/

const FILES = ['styles.css', 'landing.css', 'docs.css', 'console.css', 'faq.css'] as const;
const sources = Object.fromEntries(
  FILES.map((name) => [name, readFileSync(new URL(`./${name}`, import.meta.url), 'utf8')]),
) as Record<(typeof FILES)[number], string>;

interface Rule {
  file: string;
  selector: string;
  body: string;
}

/** The leaf rules of a stylesheet (at-rules are walked into), with comments removed. */
function rulesOf(file: string, css: string): Rule[] {
  const source = css.replace(/\/\*[\s\S]*?\*\//g, '');
  const rules: Rule[] = [];
  const walk = (text: string) => {
    let index = 0;
    while (index < text.length) {
      const open = text.indexOf('{', index);
      if (open === -1) break;
      let depth = 1;
      let close = open + 1;
      while (depth > 0 && close < text.length) {
        if (text[close] === '{') depth += 1;
        else if (text[close] === '}') depth -= 1;
        close += 1;
      }
      const selector = text.slice(index, open).trim();
      const body = text.slice(open + 1, close - 1);
      if (body.includes('{')) walk(body);
      else rules.push({ file, selector, body });
      index = close;
    }
  };
  walk(source);
  return rules;
}

const rules = FILES.flatMap((name) => rulesOf(name, sources[name]));
const root = rules.find((rule) => rule.selector === ':root');
assert.ok(root, 'styles.css has a :root rule');

function token(name: string): string {
  const match = new RegExp(`--${name}:\\s*([^;]+);`).exec(root!.body);
  assert.ok(match, `--${name} is defined`);
  return match[1]!.trim().toLowerCase().replace(/\s+/g, ' ');
}

type Rgb = [number, number, number];

function parse(value: string): { rgb: Rgb; alpha: number } {
  const hex = /^#([0-9a-f]{6})$/.exec(value);
  if (hex) {
    const n = parseInt(hex[1]!, 16);
    return { rgb: [(n >> 16) & 255, (n >> 8) & 255, n & 255], alpha: 1 };
  }
  const rgba = /^rgba\((\d+), (\d+), (\d+), ([\d.]+)\)$/.exec(value);
  assert.ok(rgba, `cannot parse ${value}`);
  return { rgb: [Number(rgba[1]), Number(rgba[2]), Number(rgba[3])], alpha: Number(rgba[4]) };
}

function over(top: { rgb: Rgb; alpha: number }, base: Rgb): Rgb {
  return top.rgb.map((channel, i) => Math.round(top.alpha * channel + (1 - top.alpha) * base[i]!)) as Rgb;
}

function luminance([r, g, b]: Rgb): number {
  const f = (v: number) => {
    const c = v / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}

function contrast(a: Rgb, b: Rgb): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x) as [number, number];
  return (hi + 0.05) / (lo + 0.05);
}

const solid = (name: string): Rgb => parse(token(name)).rgb;

describe('the dark tokens', () => {
  it('are the specified values', () => {
    const expected: Record<string, string> = {
      bg: '#121410',
      'bg-raised': '#1a1d16',
      'bg-inset': '#0e100b',
      ink: '#e4e6da',
      'ink-2': '#9da08f',
      'ink-3': '#6b6e60',
      line: 'rgba(228, 230, 218, 0.12)',
      'line-2': 'rgba(228, 230, 218, 0.22)',
      allow: '#5bbe7e',
      'allow-bg': 'rgba(91, 190, 126, 0.14)',
      deny: '#e06a4c',
      'deny-bg': 'rgba(224, 106, 76, 0.12)',
      live: '#ffc93c',
      'live-bg': 'rgba(255, 201, 60, 0.12)',
    };
    for (const [name, value] of Object.entries(expected)) assert.equal(token(name), value, name);
  });
});

describe('contrast (WCAG AA, 4.5:1 for text)', () => {
  const surfaces = ['bg', 'bg-raised', 'bg-inset'] as const;

  it('--ink and --ink-2 clear AA on every dark surface', () => {
    for (const text of ['ink', 'ink-2']) {
      for (const surface of surfaces) {
        const ratio = contrast(solid(text), solid(surface));
        assert.ok(ratio >= 4.5, `${text} on ${surface}: ${ratio.toFixed(2)}`);
      }
    }
  });

  it('text on a translucent panel, and on the hover tint, still clears AA', () => {
    const panel = over({ rgb: solid('bg-raised'), alpha: 0.86 }, solid('bg'));
    for (const text of ['ink', 'ink-2']) {
      assert.ok(contrast(solid(text), panel) >= 4.5, `${text} on the panel`);
      const hover = over(parse(token('live-bg')), panel);
      assert.ok(contrast(solid(text), hover) >= 4.5, `${text} on the hover tint`);
    }
  });

  it('the verdict stamps clear AA on the inset surface they sit on', () => {
    for (const kind of ['allow', 'deny']) {
      const tint = over(parse(token(`${kind}-bg`)), solid('bg-inset'));
      const ratio = contrast(solid(kind), tint);
      assert.ok(ratio >= 4.5, `${kind}: ${ratio.toFixed(2)}`);
    }
  });

  it('a verified value in the allow colour clears AA on the raised panel', () => {
    assert.ok(contrast(solid('allow'), solid('bg-raised')) >= 4.5);
  });

  it('yellow on the page, and the page colour on solid yellow (the hover fill), clear AA', () => {
    assert.ok(contrast(solid('live'), solid('bg')) >= 4.5);
    assert.ok(contrast(solid('bg'), solid('live')) >= 4.5);
    const primary = over(parse(token('live-bg')), solid('bg'));
    assert.ok(contrast(solid('ink'), primary) >= 4.5, 'ink on the primary button');
  });

  it('--ink-3 never sets text, because it does not reach 4.5:1', () => {
    assert.ok(contrast(solid('ink-3'), solid('bg')) < 4.5, 'if this changes, the rule can be relaxed');
    for (const rule of rules) {
      assert.ok(!/(^|[\s;])color:\s*var\(--ink-3\)/.test(rule.body), `${rule.file}: ${rule.selector}`);
    }
  });
});

describe('where each colour may appear', () => {
  const leaf = rules.filter((rule) => rule !== root);

  it('green and rust only on verdicts and verified values', () => {
    for (const rule of leaf) {
      if (/var\(--(allow|deny)\b/.test(rule.body)) {
        assert.match(rule.selector, /\.stamp--(allow|deny)|\.is-verified/, rule.selector);
      }
    }
  });

  it('yellow only on interaction and liveness', () => {
    const allowed = /:hover|:focus|aria-current|\.live-dot|::selection|\.btn--primary|\.skip/;
    for (const rule of leaf) {
      if (/var\(--live(-bg)?\b/.test(rule.body)) assert.match(rule.selector, allowed, rule.selector);
    }
  });

  it('no shadows, and rounded corners only on the round status dots', () => {
    for (const rule of leaf) {
      assert.ok(!/box-shadow|text-shadow/.test(rule.body), `${rule.selector}: shadow`);
      if (/border-radius/.test(rule.body)) {
        assert.match(rule.selector, /\.live-dot|\.chip__dot/, `${rule.selector}: rounded`);
      }
    }
  });
});

describe('the app shell layout', () => {
  const shell = rules.filter((rule) => rule.file === 'styles.css');
  const forSelector = (pattern: RegExp) => shell.filter((rule) => pattern.test(rule.selector));

  it('sets the boundary label without a transform, filter or layer that would blur it', () => {
    const label = forSelector(/^\.rail__label$/);
    assert.ok(label.length >= 2, 'the label has its rules');
    for (const rule of label) {
      assert.ok(!/(^|[\s;])(transform|filter|backdrop-filter|will-change|perspective|opacity)\s*:/.test(rule.body), rule.selector);
    }
  });

  it('sizes the boundary label in whole pixels', () => {
    const main = forSelector(/^\.rail__label$/)[0]!.body;
    for (const property of ['font-size', 'line-height', 'padding']) {
      const value = new RegExp(`${property}:\\s*([^;]+);`).exec(main)?.[1];
      assert.ok(value, property);
      assert.match(value, /^(\d+px\s*)+$/, `${property} is whole pixels, found ${value}`);
    }
  });

  it('starts both zones at the same height, as rows of one shared grid', () => {
    for (const rule of forSelector(/^\.zone(--agent|--money)?$/)) {
      assert.ok(!/(^|[\s;])(padding-top|margin-top|padding-block|transform|align-self)\s*:/.test(rule.body), rule.selector);
    }
    const stage = forSelector(/^\.stage$/)[0]!.body;
    assert.match(stage, /grid-template-rows:\s*auto auto auto 1fr/);
    const subgrid = forSelector(/^\.zone$/).find((rule) => rule.body.includes('subgrid'));
    assert.ok(subgrid, 'the zones share the stage rows through a subgrid');
    assert.match(subgrid.body, /grid-row:\s*1 \/ span 4/);
  });
});

describe('the stylesheets do not collide', () => {
  const standalone = (pattern: RegExp) =>
    rules.filter((rule) => rule.file !== undefined && pattern.test(rule.selector) && !rule.selector.includes(','));

  it('define no class more than once across files: one file cannot restyle another file\'s element', () => {
    const owner = new Map<string, string>();
    for (const rule of rules) {
      for (const selector of rule.selector.split(',').map((part) => part.trim())) {
        const match = /^\.([a-z][a-z0-9_-]*)$/.exec(selector);
        if (!match) continue;
        const previous = owner.get(match[1]!);
        assert.ok(previous === undefined || previous === rule.file, `.${match[1]} is styled in both ${previous} and ${rule.file}`);
        owner.set(match[1]!, rule.file);
      }
    }
    assert.ok(standalone(/^\.[a-z]/).length > 50, 'the check saw the stylesheets');
  });

  it('puts nothing outside the flow except what is meant to be', () => {
    // Every rule that positions an element absolutely or fixed. An element that is not here cannot leave its
    // container and pile up over the header; a new one has to be added here on purpose.
    const outOfFlow = rules
      .filter((rule) => /(^|[\s;])position:\s*(absolute|fixed)/.test(rule.body))
      .map((rule) => rule.selector)
      .sort();
    assert.deepEqual(outOfFlow, [
      '.chain__link + .chain__link::before',
      '.field',
      '.live-dot::after',
      '.method input',
      '.rail__label',
      '.rail__ticks',
      '.skip',
      '.sr-only',
    ]);
  });

  it('keeps the one fixed element the sensing canvas', () => {
    const fixed = rules.filter((rule) => /(^|[\s;])position:\s*fixed/.test(rule.body)).map((rule) => rule.selector);
    assert.deepEqual(fixed, ['.field']);
  });
});
