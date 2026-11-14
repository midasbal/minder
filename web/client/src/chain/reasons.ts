/** The contract's own deny codes, with one plain line each. The code is always shown beside the line. */

const REASONS: Readonly<Record<string, string>> = {
  Frozen: 'The contract is frozen: nothing moves until the owner unfreezes it.',
  NoMandate: 'There is no mandate, so the agent has no authority at all.',
  MandateRevoked: 'The mandate has been revoked.',
  MandateExpired: 'The mandate has expired.',
  NotAuthorized: 'This key is not the agent key the mandate names.',
  AssetNotAllowed: 'This asset has no policy, so it cannot be moved.',
  RecipientNotAllowed: "The recipient is not on the owner's allowlist.",
  PerTxCapExceeded: 'This one request is larger than the per-transaction cap.',
  WindowLimitExceeded: 'With what was spent recently, this would pass the rolling-window limit.',
  AssetOutOfScope: "The agent's mandate does not cover this asset.",
  RecipientOutOfScope: "The agent's mandate does not cover this recipient.",
  MandateBudgetExceeded: "This would pass the budget for this grant of the agent's mandate.",
};

export function reasonLine(code: string): string {
  return REASONS[code] ?? 'The contract returned a reason this page has no description for.';
}
