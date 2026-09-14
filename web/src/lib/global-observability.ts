// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export const OBSERVABILITY_PAGE_SIZE = 25
export { QUICK_TIME_RANGES as OBSERVABILITY_RANGES } from './date-time-range'
import { QUICK_TIME_RANGES, type DateTimeRangeValue } from './date-time-range'
export type ObservabilityRange = DateTimeRangeValue['preset']

export function positiveInteger(value: string | null): number | undefined {
  if (!value || !/^\d+$/.test(value)) return undefined
  const number = Number(value)
  return Number.isSafeInteger(number) && number > 0 ? number : undefined
}

export function readObservationWindow(params: URLSearchParams, now: number) {
  const requested = params.get('range') ?? '1d'
  const rangeParam = requested === '24h' ? '1d' : requested
  const preset = Object.prototype.hasOwnProperty.call(
    QUICK_TIME_RANGES,
    rangeParam
  )
    ? (rangeParam as keyof typeof QUICK_TIME_RANGES)
    : undefined
  const from = Date.parse(params.get('from') ?? '')
  const to = Date.parse(params.get('to') ?? '')
  const valid =
    Number.isFinite(from) &&
    Number.isFinite(to) &&
    from < to &&
    to - from <= 30 * 86400000
  // Keep legacy/custom absolute links accurate instead of marking a wrong quick action active.
  const range: ObservabilityRange =
    valid && (!preset || to - from !== QUICK_TIME_RANGES[preset] * 3600000)
      ? 'custom'
      : (preset ?? '1d')
  return {
    range,
    from: new Date(
      valid
        ? from
        : now - QUICK_TIME_RANGES[range === 'custom' ? '1d' : range] * 3600000
    ).toISOString(),
    to: new Date(valid ? to : now).toISOString(),
  }
}

/** Cursor tokens are bound to the entire query, including its frozen time window. */
export function patchObservationFilters(
  current: URLSearchParams,
  patch: Record<string, string | undefined>
) {
  const next = new URLSearchParams(current)
  next.delete('page')
  next.delete('cursor')
  for (const [key, value] of Object.entries(patch)) {
    if (value) next.set(key, value)
    else next.delete(key)
  }
  return next
}

export function observationError(error: unknown): string {
  if (error && typeof error === 'object') {
    if ('detail' in error && typeof error.detail === 'string')
      return error.detail
    if ('title' in error && typeof error.title === 'string') return error.title
    if ('message' in error && typeof error.message === 'string')
      return error.message
  }
  return 'The request could not be completed. Check your connection and access, then retry.'
}

export const number = (value: number | null | undefined) =>
  value == null
    ? '—'
    : new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 }).format(
        value
      )
