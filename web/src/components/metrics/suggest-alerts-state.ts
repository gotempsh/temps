export const focusedStartPrompt = (metric: string) =>
  `I'm looking at the metric \`${metric}\`. Query its real values, decide whether it's worth alerting on, and if so propose a rule with a threshold grounded in what you find (backtest it first). Check what this project already alerts on so you don't duplicate an existing rule. Afterwards, mention briefly if any other reported metric obviously deserves an alert too.`

export type SuggestAlertsState =
  'loading' | 'configure-provider' | 'enable-writes' | 'ready'

export function suggestAlertsState(input: {
  isPending: boolean
  isError: boolean
  aiConfigured?: boolean
  writeEnabled?: boolean
}): SuggestAlertsState {
  if (input.isPending) return 'loading'
  if (input.isError) return 'ready'
  if (!input.aiConfigured) return 'configure-provider'
  if (!input.writeEnabled) return 'enable-writes'
  return 'ready'
}

export const ENABLE_AI_WRITES_BODY = {
  ai_write_actions_enabled: true,
  ai_debug_chat_enabled: true,
} as const
