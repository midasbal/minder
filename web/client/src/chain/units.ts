/** Decimal text to and from integer base units, exactly: no floating point is involved. */

const U128_MAX = (1n << 128n) - 1n;

export type Parsed = { ok: true; value: bigint } | { ok: false; error: string };

/** `"0.05"` with 24 decimals is 50000000000000000000000. Refuses more precision than the asset has. */
export function parseUnits(text: string, decimals: number, symbol: string): Parsed {
  const match = /^(\d+)(?:\.(\d+))?$/.exec(text.trim());
  if (!match) return { ok: false, error: 'Enter a plain decimal number, such as 0.05' };
  const [, whole = '', fraction = ''] = match;
  if (fraction.length > decimals) return { ok: false, error: `${symbol} has ${decimals} decimal places` };
  const value = BigInt(whole + fraction.padEnd(decimals, '0'));
  if (value > U128_MAX) return { ok: false, error: 'That is larger than the contract can count' };
  return { ok: true, value };
}

/** 50000000000000000000000 with 24 decimals is `"0.05"`: trailing zeros trimmed. */
export function formatUnits(value: bigint, decimals: number): string {
  const digits = value.toString().padStart(decimals + 1, '0');
  const whole = digits.slice(0, digits.length - decimals);
  const fraction = digits.slice(digits.length - decimals).replace(/0+$/, '');
  return fraction === '' ? whole : `${whole}.${fraction}`;
}
