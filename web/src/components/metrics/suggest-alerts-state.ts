// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export const focusedStartPrompt = (metric: string) =>
  `I'm looking at the metric \`${metric}\`. Query its real values, decide whether it's worth alerting on, and if so propose a rule with a threshold grounded in what you find (backtest it first). Check what this project already alerts on so you don't duplicate an existing rule. Afterwards, mention briefly if any other reported metric obviously deserves an alert too.`

export type SuggestAlertsState = 'loading' | 'configure-provider' | 'ready'

export function suggestAlertsState(input: {
  isPending: boolean
  isError: boolean
  aiConfigured?: boolean
}): SuggestAlertsState {
  if (input.isPending) return 'loading'
  if (input.isError) return 'ready'
  if (!input.aiConfigured) return 'configure-provider'
  return 'ready'
}
