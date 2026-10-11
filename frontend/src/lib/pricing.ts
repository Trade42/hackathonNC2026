// Campus pickup pricing. Keep these values in sync with ferry-core's money math.
export const RUNNER_BASE_CENTS = 200
export const FERRY_BASE_CENTS = 299

export function ferryFeeCents(): number {
  return FERRY_BASE_CENTS
}

// Older orders paid their entire fee to the runner. Their stored fee is below
// the current base, so this also preserves their historical payout.
export function runnerPayCents(feeCents: number, tipCents: number): number {
  return Math.min(feeCents, RUNNER_BASE_CENTS) + tipCents
}

export function platformShareCents(feeCents: number): number {
  return Math.max(0, feeCents - RUNNER_BASE_CENTS)
}
