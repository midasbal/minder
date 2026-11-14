/*
  One-click attacks, worked out from the live policy so they stay true when the owner changes it.

  A preset only fills in the form. It names what the attacker tries, never what will happen: the verdict is
  the contract's, and it is read when the request is submitted.
*/

import { assetOfMethod, formatAsset, methodSpec, type MethodId } from './assets';
import type { Snapshot } from './snapshot';
import { formatUnits } from './units';

export interface Preset {
  id: 'over-cap' | 'unlisted-recipient' | 'whole-window' | 'in-policy';
  label: string;
  /** What the attacker is trying, in a line. */
  tries: string;
  recipient: string;
  amount: string;
}

/** A recipient on no list: the ordinary NEAR sink for dust, and the EVM burn address. */
const UNLISTED = { near: 'not-on-the-list.testnet', evm: '0x000000000000000000000000000000000000dEaD' } as const;

/** The presets for a method, or why there are none yet. */
export function presetsFor(method: MethodId, snapshot: Snapshot | null): { presets: Preset[]; unavailable: null } | { presets: []; unavailable: string } {
  if (!snapshot) return { presets: [], unavailable: 'Reading the live policy…' };
  if (!snapshot.assets.ok) return { presets: [], unavailable: "The live policy could not be read, so the presets can't be set." };
  const spec = methodSpec(method);
  const row = snapshot.assets.value.find((candidate) => assetOfMethod(method, candidate.asset));
  if (!row) return { presets: [], unavailable: 'The contract has no policy for this asset.' };
  const recipients = spec.recipient === 'near' ? snapshot.nearRecipients : snapshot.evmRecipients;
  if (!recipients.ok) return { presets: [], unavailable: "The allowlist could not be read, so the presets can't be set." };
  const listed = recipients.value[0];
  if (listed === undefined) return { presets: [], unavailable: 'The allowlist is empty, so there is no legitimate recipient.' };

  const show = (units: bigint) => formatUnits(units, spec.decimals);
  const small = row.perTxCap / 20n;
  return {
    unavailable: null,
    presets: [
      {
        id: 'over-cap',
        label: 'Over the cap',
        tries: `Pay ${formatAsset(row.asset, row.perTxCap * 2n)}, twice the per-transaction cap, to a listed recipient.`,
        recipient: listed,
        amount: show(row.perTxCap * 2n),
      },
      {
        id: 'unlisted-recipient',
        label: 'Unlisted recipient',
        tries: `Pay a small amount to an address the owner never approved.`,
        recipient: UNLISTED[spec.recipient],
        amount: show(small),
      },
      {
        id: 'whole-window',
        label: 'Drain the window',
        tries: `Take the whole window limit, ${formatAsset(row.asset, row.windowLimit)}, in one request.`,
        recipient: listed,
        amount: show(row.windowLimit),
      },
      {
        id: 'in-policy',
        label: 'In policy',
        tries: `Pay ${formatAsset(row.asset, small)}, a twentieth of the cap, to a listed recipient.`,
        recipient: listed,
        amount: show(small),
      },
    ],
  };
}
