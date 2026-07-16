export type InsightTone = 'positive' | 'negative' | 'neutral'

export type InsightSource = 'stat' | 'ai'

/**
 * One insight rendered by `InsightsPanel`. Stat insights are derived
 * client-side from data the page already fetched (see `derive.ts`).
 * AI analysis goes through the project's AI chat (the assistant dock),
 * seeded with the same stats — see `InsightsPanel`.
 */
export interface Insight {
  id: string
  source: InsightSource
  tone: InsightTone
  /** Short, concrete claim, e.g. "Chrome carries most of your traffic." */
  title: string
  /** One supporting sentence with the numbers behind the claim. */
  detail?: string
  /** Headline figure shown on the right, e.g. "68%" or "4 PM". */
  value?: string
}

/**
 * Stats context handed to the AI assistant when the user asks for AI
 * insights. The chat's opening prompt embeds this summary so the model
 * starts from the numbers already on screen instead of inventing them.
 */
export interface AiInsightContext {
  /** Which analytics surface the stats describe, e.g. "top pages". */
  surface: string
  rangeStart?: string
  rangeEnd?: string
  stats: Record<string, unknown>
}
