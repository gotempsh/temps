// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'

/**
 * Pure helpers for the AI gateway limits console.
 *
 * Limits are stored with three states per field, and the console has to keep
 * them distinct:
 *   - `null`  -> inherit from the next scope (project -> instance -> built-in)
 *   - `0`     -> unlimited
 *   - `n > 0` -> a concrete cap
 * The budget is stored in microcents (1 USD = 1_000_000), the same unit the
 * usage log records spend in.
 */

export const MICROCENTS_PER_USD = 1_000_000

/** Server-side ceiling for `max_requests_per_minute`. */
export const MAX_REQUESTS_PER_MINUTE = 1_000_000

export type LimitSource = 'project' | 'instance' | 'built_in'

export function usdToMicrocents(usd: number): number {
  return Math.round(usd * MICROCENTS_PER_USD)
}

export function microcentsToUsd(microcents: number): number {
  return microcents / MICROCENTS_PER_USD
}

const usdFormatter = new Intl.NumberFormat('en-US', {
  style: 'currency',
  currency: 'USD',
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
})

const smallUsdFormatter = new Intl.NumberFormat('en-US', {
  style: 'currency',
  currency: 'USD',
  minimumFractionDigits: 4,
  maximumFractionDigits: 4,
})

export function formatMicrocents(microcents: number): string {
  const usd = microcentsToUsd(microcents)
  // Sub-cent spend is common for small models; showing "$0.00" would read as
  // "nothing happened".
  if (usd > 0 && usd < 0.01) return smallUsdFormatter.format(usd)
  return usdFormatter.format(usd)
}

/** `null` means "inherit", so it has no display of its own. */
export function formatRequestLimit(value: number | null | undefined): string {
  if (value === null || value === undefined) return 'Inherited'
  if (value === 0) return 'Unlimited'
  return `${value.toLocaleString('en-US')} / min`
}

export function formatBudgetLimit(value: number | null | undefined): string {
  if (value === null || value === undefined) return 'Inherited'
  if (value === 0) return 'Unlimited'
  return `${formatMicrocents(value)} / month`
}

/** What the gateway enforces after resolution; `null` is "no limit". */
export function formatEffectiveRequestLimit(value: number | null): string {
  return value === null ? 'Unlimited' : `${value.toLocaleString('en-US')} / min`
}

export function formatEffectiveBudget(value: number | null): string {
  return value === null ? 'Unlimited' : `${formatMicrocents(value)} / month`
}

/**
 * Instance-scope display. The instance has no scope above it, so an unset
 * request limit falls to the built-in default and an unset budget means no cap.
 */
export function describeInstanceRequestLimit(
  stored: number | null | undefined,
  builtIn: number
): { value: string; note: string } {
  if (stored === null || stored === undefined) {
    return {
      value: formatEffectiveRequestLimit(builtIn),
      note: 'Built-in default',
    }
  }
  return {
    value: stored === 0 ? 'Unlimited' : formatEffectiveRequestLimit(stored),
    note: 'Set by an administrator',
  }
}

export function describeInstanceBudget(stored: number | null | undefined): {
  value: string
  note: string
} {
  if (stored === null || stored === undefined) {
    return { value: 'Unlimited', note: 'No budget set' }
  }
  return {
    value: stored === 0 ? 'Unlimited' : formatEffectiveBudget(stored),
    note: 'Set by an administrator',
  }
}

export function describeLimitSource(source: LimitSource): string {
  switch (source) {
    case 'project':
      return 'Project override'
    case 'instance':
      return 'Instance default'
    case 'built_in':
      return 'Built-in default'
  }
}

/** Share of the budget consumed, clamped to 0-100 for a progress bar. */
export function budgetUsedPercent(
  spendMicrocents: number,
  budgetMicrocents: number | null
): number | null {
  if (budgetMicrocents === null || budgetMicrocents <= 0) return null
  return Math.min(100, Math.max(0, (spendMicrocents / budgetMicrocents) * 100))
}

/**
 * The editable form shape. Inputs are strings so an empty box can mean
 * "inherit" without a sentinel number.
 */
export interface LimitFormValues {
  requestsPerMinute: string
  monthlyBudgetUsd: string
}

export interface LimitPayload {
  max_requests_per_minute: number | null
  max_cost_per_month_microcents: number | null
}

export type LimitFormError = Partial<Record<keyof LimitFormValues, string>>

export function limitsToFormValues(limits: {
  max_requests_per_minute?: number | null
  max_cost_per_month_microcents?: number | null
}): LimitFormValues {
  const rpm = limits.max_requests_per_minute
  const cost = limits.max_cost_per_month_microcents
  return {
    requestsPerMinute: rpm === null || rpm === undefined ? '' : String(rpm),
    monthlyBudgetUsd:
      cost === null || cost === undefined ? '' : String(microcentsToUsd(cost)),
  }
}

/**
 * Validate the form and convert it to the wire shape. Returns either the
 * payload or per-field messages; never both.
 */
export function parseLimitForm(
  values: LimitFormValues
): { ok: true; payload: LimitPayload } | { ok: false; errors: LimitFormError } {
  const errors: LimitFormError = {}
  let rpm: number | null = null
  let cost: number | null = null

  const rpmText = values.requestsPerMinute.trim()
  if (rpmText !== '') {
    const parsed = Number(rpmText)
    if (!Number.isInteger(parsed) || parsed < 0) {
      errors.requestsPerMinute = 'Enter a whole number, 0 or greater.'
    } else if (parsed > MAX_REQUESTS_PER_MINUTE) {
      errors.requestsPerMinute = `Must be at most ${MAX_REQUESTS_PER_MINUTE.toLocaleString('en-US')}.`
    } else {
      rpm = parsed
    }
  }

  const costText = values.monthlyBudgetUsd.trim()
  if (costText !== '') {
    const parsed = Number(costText)
    if (!Number.isFinite(parsed) || parsed < 0) {
      errors.monthlyBudgetUsd = 'Enter an amount in USD, 0 or greater.'
    } else {
      cost = usdToMicrocents(parsed)
    }
  }

  if (Object.keys(errors).length > 0) return { ok: false, errors }
  return {
    ok: true,
    payload: {
      max_requests_per_minute: rpm,
      max_cost_per_month_microcents: cost,
    },
  }
}

/**
 * Zod schema for React Hook Form. It delegates to `parseLimitForm` so the
 * form and the wire conversion can never disagree about what is valid.
 */
export const limitFormSchema = z
  .object({
    requestsPerMinute: z.string(),
    monthlyBudgetUsd: z.string(),
  })
  .superRefine((values, ctx) => {
    const result = parseLimitForm(values)
    if (result.ok) return
    for (const [field, message] of Object.entries(result.errors)) {
      ctx.addIssue({ code: 'custom', path: [field], message })
    }
  })

/** Start of the current UTC month: the window the monthly budget counts. */
export function monthStartIso(now: Date): string {
  return new Date(
    Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1)
  ).toISOString()
}

/**
 * The override page to show after one row is removed. Removing the only row on
 * a later page would leave it empty, so step back to the previous page.
 */
export function pageAfterRemoval(page: number, rowsOnPage: number): number {
  return rowsOnPage <= 1 ? Math.max(1, page - 1) : page
}
