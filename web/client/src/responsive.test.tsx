import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { describe, it } from 'node:test';

/*
  The layout, checked against the stylesheets themselves. There is no browser here, so these tests pin the rules the
  responsive design rests on: nothing can be wider than its container, every multi-column layout has a way down to one
  column, wide content scrolls inside its own box, and touch screens get usable targets.
*/

const FILES = ['styles.css', 'landing.css', 'docs.css', 'console.css', 'faq.css'] as const;
const css = Object.fromEntries(FILES.map((name) => [name, readFileSync(new URL(`./${name}`, import.meta.url), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '')])) as Record<(typeof FILES)[number], string>;
const all = FILES.map((name) => css[name]).join('\n');

interface Rule {
  /** The at-rule it sits in, such as `@media (max-width: 40rem)`, or '' at the top level. */
  within: string;
  selector: string;
  body: string;
}

function blocks(source: string, within = ''): Rule[] {
  const rules: Rule[] = [];
  let index = 0;
  while (index < source.length) {
    const open = source.indexOf('{', index);
    if (open === -1) break;
    let depth = 1;
    let close = open + 1;
    while (depth > 0 && close < source.length) {
      if (source[close] === '{') depth += 1;
      else if (source[close] === '}') depth -= 1;
      close += 1;
    }
    const head = source.slice(index, open).trim();
    const body = source.slice(open + 1, close - 1);
    if (head.startsWith('@media') || head.startsWith('@container') || head.startsWith('@supports')) rules.push(...blocks(body, head));
    else if (!head.startsWith('@')) rules.push({ within, selector: head, body });
    index = close;
  }
  return rules;
}

const rules = FILES.flatMap((name) => blocks(css[name]));
const declares = (rule: Rule, property: string) => new RegExp(`(^|[\\s;])${property}\\s*:`).test(rule.body);
const find = (selector: string, within?: RegExp) => rules.filter((rule) => rule.selector.split(',').map((part) => part.trim()).includes(selector) && (within ? within.test(rule.within) : true));
/** The value of a property from the first rule for the selector that sets it. */
const prop = (selector: string, property: string, within?: RegExp) => {
  const rule = find(selector, within).find((candidate) => declares(candidate, property));
  assert.ok(rule, `${selector} sets ${property}`);
  return value(rule, property)!;
};
const value = (rule: Rule, property: string) => new RegExp(`(?:^|[\\s;])${property}\\s*:\\s*([^;]+);`).exec(rule.body)?.[1]?.trim();

describe('nothing can be wider than the box it is in', () => {
  it('caps every automatic grid track at the width of its container', () => {
    const floors = [...all.matchAll(/repeat\(auto-(?:fit|fill),\s*minmax\((min\(100%,[^)]*\)|[^,()]+),/g)].map((match) => match[1]!.trim());
    assert.ok(floors.length >= 6, 'the check saw the grids');
    for (const floor of floors) assert.match(floor, /^min\(100%,/, `an automatic grid floor of ${floor} would overflow a narrow container`);
  });

  it('lets every grid column shrink: no track is a bare fraction with an unbounded minimum', () => {
    for (const rule of rules) {
      const columns = value(rule, 'grid-template-columns');
      if (!columns) continue;
      for (const track of columns.match(/(?<![\w(-])\d*\.?\d+fr/g) ?? []) {
        assert.ok(/minmax\(0,\s*[\d.]*fr\)/.test(columns) || columns.trim() === '1fr' || columns.includes('auto-fit') || columns.includes('1fr auto') || columns.includes('auto 1fr'),
          `${rule.selector}: ${track} in "${columns}" can hold a track open at its content's width`);
      }
    }
  });

  it('keeps the whole page from scrolling sideways, as a last resort, without disturbing the sticky header', () => {
    const shell = find('.shell')[0]!;
    assert.equal(value(shell, 'overflow-x'), 'clip');
    assert.ok(!declares(shell, 'overflow') && !/overflow-x:\s*(hidden|auto|scroll)/.test(shell.body), 'clip, not a scroll container');
  });

  it('lets grid and flex children shrink below their content', () => {
    for (const selector of ['.panel', '.zone', '.gate-wrap', '.docs__body', '.payment', '.proof', '.rt-attack', '.safe']) {
      assert.ok(find(selector).some((rule) => value(rule, 'min-width') === '0'), `${selector} has min-width: 0`);
    }
  });
});

describe('wide content scrolls inside its own box', () => {
  it('puts the policy table, the docs tables, the code blocks and the raw hex in boxes that scroll', () => {
    for (const [selector, property] of [['.ptable-wrap', 'overflow-x'], ['.table-wrap', 'overflow-x'], ['.pre', 'overflow-x'], ['.raw__code', 'overflow']] as const) {
      const rule = find(selector).find((candidate) => declares(candidate, property));
      assert.ok(rule && /auto/.test(value(rule, property)!), `${selector} scrolls`);
    }
  });

  it('breaks long hashes and addresses instead of widening the page', () => {
    assert.match(value(find('code')[0]!, 'overflow-wrap')!, /anywhere/);
    assert.match(value(find('.break')[0]!, 'overflow-wrap')!, /anywhere/);
    assert.match(value(find('.chain__address code')[0]!, 'overflow-wrap')!, /anywhere/);
    assert.match(value(find('.panel--live .kv__row dd')[0]!, 'overflow-wrap')!, /anywhere/);
  });

  it('lets the keyboard reach the scrollable boxes', () => {
    const live = readFileSync(new URL('./components/console/LivePanel.tsx', import.meta.url), 'utf8');
    assert.match(live, /className="ptable-wrap" role="region" aria-label="[^"]+" tabIndex=\{0\}/);
    for (const file of ['RawCall', 'RealRun', 'RedTeam']) {
      const source = readFileSync(new URL(`./components/console/${file}.tsx`, import.meta.url), 'utf8');
      assert.ok(!/<pre className="raw__code">/.test(source), `${file}: every raw__code block is focusable`);
      assert.ok(/<pre className="raw__code" tabIndex=\{0\}>/.test(source), file);
    }
  });
});

const collapsesTo = (selector: string, within: RegExp) =>
  assert.ok(find(selector, within).some((rule) => /^(minmax\(0,\s*1fr\)|1fr)$/.test(value(rule, 'grid-template-columns') ?? '')), `${selector} becomes one column ${within}`);

describe('every layout with columns has a way down to one', () => {
  const collapses = collapsesTo;

  it('collapses the hero, the footer, the header, the stage and the docs on narrow screens', () => {
    collapses('.hero', /max-width: 62rem/);
    collapses('.site-footer__inner', /max-width: 54rem/);
    collapses('.stage', /max-width: 54rem/);
    collapses('.docs', /max-width: 64rem/);
    assert.equal(value(find('.site-header__inner', /max-width: 54rem/)[0]!, 'grid-template-columns'), '1fr auto', 'the header drops its nav to a second row');
    assert.ok(find('.nav', /max-width: 54rem/).some((rule) => value(rule, 'overflow-x') === 'auto'), 'the nav scrolls instead of wrapping');
  });

  it('collapses the gate by its own width, since in the hero it is half a screen wide', () => {
    assert.match(value(find('.gate-wrap')[0]!, 'container')!, /^gate \/ inline-size$/);
    collapses('.gate__body', /@container gate \(max-width: 34rem\)/);
    assert.ok(find('.decision', /@container gate/).some((rule) => value(rule, 'flex-wrap') === 'wrap'));
  });

  it('collapses the console panel\'s presets and verdict labels by the panel\'s own width', () => {
    assert.match(value(find('.panel')[0]!, 'container')!, /^panel \/ inline-size$/);
    collapses('.presets', /@container panel \(max-width: 30rem\)/);
    assert.ok(find('.verdict__tag', /@container panel/).length > 0);
  });

  it('collapses the framing, the datasheets and the timeline', () => {
    collapses('.tracks', /max-width: 40rem/);
    collapses('.fd-step', /max-width: 40rem/);
    assert.match(value(find('.sheets')[0]!, 'grid-template-columns')!, /^repeat\(auto-fit, minmax\(min\(100%, 19rem\), 1fr\)\)$/);
  });

  it('turns the docs contents into a wrapped row on small screens', () => {
    const toc = find('.toc', /max-width: 64rem/)[0]!;
    assert.equal(value(toc, 'display'), 'flex');
    assert.equal(value(toc, 'flex-wrap'), 'wrap');
  });
});

describe('small and touch screens', () => {
  it('keeps the navigation on one line, scrolling rather than wrapping', () => {
    assert.equal(prop('.nav a', 'white-space'), 'nowrap');
    assert.equal(prop('.nav a', 'flex'), 'none');
  });

  it('sets form fields to 16px on touch screens, so iOS does not zoom into them', () => {
    assert.equal(value(find('.input', /pointer: coarse/)[0]!, 'font-size'), '1rem');
  });

  it('gives touch targets at least 44px', () => {
    for (const selector of ['.nav a', '.btn', '.panel__bar .btn']) {
      const rule = find(selector, /pointer: coarse/).find((candidate) => declares(candidate, 'min-height'));
      assert.ok(rule && value(rule, 'min-height') === '2.75rem', `${selector} is at least 2.75rem tall on touch`);
    }
    assert.ok(find('.method span', /pointer: coarse/).length > 0);
    assert.ok(find('.more summary', /pointer: coarse/).length > 0);
    assert.ok(find('.toc a', /pointer: coarse/).length > 0);
  });

  it('lets the header scroll away on a short screen, and clears it when jumping to a heading', () => {
    assert.equal(value(find('.site-header', /max-height: 32rem/)[0]!, 'position'), 'static');
    assert.equal(value(find('.doc-h2', /max-width: 54rem/)[0]!, 'scroll-margin-top'), '8rem');
  });

  it('keeps the smallest text legible: nothing under 0.66rem, and the tiny labels grow on a phone', () => {
    for (const match of all.matchAll(/font-size:\s*([\d.]+)rem/g)) assert.ok(Number(match[1]) >= 0.66, `font-size ${match[1]}rem is too small`);
    assert.ok(find('.safe__source', /max-width: 40rem/).some((rule) => value(rule, 'font-size') === '0.72rem'));
    assert.ok(find('.gate__label', /max-width: 40rem/).some((rule) => value(rule, 'font-size') === '0.72rem'));
  });

  it('lets people zoom: the viewport is the device width and zooming is not disabled', () => {
    const html = readFileSync(new URL('../index.html', import.meta.url), 'utf8');
    assert.match(html, /<meta name="viewport" content="width=device-width, initial-scale=1" \/>/);
    assert.ok(!/maximum-scale|user-scalable/.test(html));
  });
});

describe('the footer', () => {
  it('centres the right-hand group against the two-line brand, and stacks it on narrow screens', () => {
    assert.equal(prop('.site-footer__inner', 'align-items'), 'center');
    assert.equal(prop('.site-footer__inner', 'grid-template-columns'), '1fr auto');
    assert.equal(prop('.site-footer__inner', 'align-items', /max-width: 54rem/), 'start');
    collapsesTo('.site-footer__inner', /max-width: 54rem/);
  });

  it('spaces the links and the relay chip with one gap, so the rhythm is even', () => {
    const meta = find('.site-footer__meta')[0]!;
    const links = find('.site-footer__links').find((rule) => declares(rule, 'gap'))!;
    assert.equal(value(meta, 'gap'), value(links, 'gap'));
    assert.equal(value(meta, 'align-items'), 'center');
    assert.equal(value(meta, 'flex-wrap'), 'wrap');
  });
});

describe('the sensing field is for fine pointers and calm motion only', () => {
  const field = readFileSync(new URL('./field.ts', import.meta.url), 'utf8');

  it('follows the pointer only when it is fine and motion is welcome', () => {
    assert.ok(field.includes("matchMedia('(pointer: fine)')"));
    assert.ok(field.includes("matchMedia('(prefers-reduced-motion: no-preference)')"));
    assert.ok(/const wanted = finePointer\.matches && motionOk\.matches;/.test(field));
  });

  it('never intercepts touches or clicks', () => {
    assert.equal(value(find('.field')[0]!, 'pointer-events'), 'none');
  });
});
