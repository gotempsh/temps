// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** The slice of the usage response the spend readout needs. */
export interface ChatSpendInput {
  requests: number
  cost_microcents: number
  unpriced_requests: number
}

/** One microcent is 1/10000 of a cent, so a dollar is 1,000,000 of them. */
const MICROCENTS_PER_DOLLAR = 1_000_000

/**
 * Dollars for an estimated cost. Chats cost fractions of a cent, so small
 * amounts keep four decimals instead of rounding to a misleading `$0.00`.
 */
export function formatSpend(microcents: number): string {
  const dollars = Math.max(0, microcents) / MICROCENTS_PER_DOLLAR
  if (dollars === 0) return '$0.00'
  if (dollars < 0.0001) return '<$0.0001'
  if (dollars < 1) return `$${dollars.toFixed(4)}`
  return `$${dollars.toFixed(2)}`
}

/**
 * The headline for a chat's spend. A chat that used a model with no known
 * price is shown as a floor ("at least"), never as a complete total, and a
 * chat whose every request was unpriced says the cost is unknown.
 */
export function spendLabel(usage: ChatSpendInput): string {
  if (usage.requests === 0) return 'No spend yet'
  if (usage.unpriced_requests >= usage.requests) return 'Cost unknown'
  const amount = formatSpend(usage.cost_microcents)
  return usage.unpriced_requests > 0 ? `At least ${amount}` : amount
}

/** Compact token count: 1,234 → 1.2k, 2,500,000 → 2.5M. */
export function formatTokens(tokens: number): string {
  if (tokens < 1000) return String(tokens)
  if (tokens < 1_000_000) return `${(tokens / 1000).toFixed(1)}k`
  return `${(tokens / 1_000_000).toFixed(1)}M`
}

/** The slice of the usage response the context readout needs. */
export interface ChatContextInput {
  tokens: number
  window?: number | null
}

/** How full the model's context is, from 0 to 1, or `null` without a window. */
export function contextShare(context: ChatContextInput): number | null {
  if (!context.window || context.window <= 0) return null
  return Math.min(1, Math.max(0, context.tokens / context.window))
}

/** Where a chat stands against its model's window, for colour and advice. */
export type ContextLevel = 'ok' | 'high' | 'critical'

const HIGH_SHARE = 0.6
const CRITICAL_SHARE = 0.85

export function contextLevel(context: ChatContextInput): ContextLevel {
  const share = contextShare(context)
  if (share === null || share < HIGH_SHARE) return 'ok'
  return share < CRITICAL_SHARE ? 'high' : 'critical'
}

/** "38.5k of 1.0M tokens (4%)", or just "38.5k tokens" without a window. */
export function contextLabel(context: ChatContextInput): string {
  const share = contextShare(context)
  if (share === null || !context.window) {
    return `${formatTokens(context.tokens)} tokens`
  }
  const percent =
    share > 0 && share < 0.01 ? '<1' : String(Math.round(share * 100))
  return `${formatTokens(context.tokens)} of ${formatTokens(context.window)} tokens (${percent}%)`
}

/** What "Compact now" reported, as the API returns it. */
export interface CompactResultInput {
  compacted: boolean
  folded_messages: number
  reason?: string | null
}

/** One line for the toast after "Compact now". */
export function compactResultMessage(result: CompactResultInput): string {
  if (!result.compacted) {
    return result.reason ?? 'Nothing to compact yet.'
  }
  const count = result.folded_messages
  return `Summarized ${count} earlier ${count === 1 ? 'message' : 'messages'}. The full chat stays on screen; later replies cost less.`
}

/** "12 earlier messages are summarized for the model", or `null` for none. */
export function summarizedLabel(summarized: number): string | null {
  if (summarized <= 0) return null
  return `${summarized} earlier ${summarized === 1 ? 'message is' : 'messages are'} summarized for the model; the full chat stays on screen.`
}
