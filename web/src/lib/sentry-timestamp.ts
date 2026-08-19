import type { SentryTimestamp } from '@/types/sentry'
import { isValid, parseISO } from 'date-fns'

const RFC3339_TIMEZONE = /(?:Z|[+-]\d{2}:\d{2})$/i
const NUMERIC_STRING = /^[+-]?\d+(?:\.\d+)?$/

/**
 * Parse a Sentry protocol timestamp.
 *
 * Sentry accepts RFC3339 strings or numeric Unix timestamps in seconds.
 * When an RFC3339 string omits a timezone, Sentry treats it as UTC.
 */
export function parseSentryTimestamp(
  value: SentryTimestamp | null | undefined
): Date | null {
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) {
      return null
    }

    const date = new Date(value * 1000)
    return isValid(date) ? date : null
  }

  if (typeof value !== 'string') {
    return null
  }

  const input = value.trim()
  if (!input || NUMERIC_STRING.test(input)) {
    return null
  }

  // Sentry assumes UTC when an RFC3339 timestamp omits the timezone.
  const normalized = RFC3339_TIMEZONE.test(input) ? input : `${input}Z`
  const date = parseISO(normalized)

  return isValid(date) ? date : null
}

export function sentryTimestampToMillis(
  value: SentryTimestamp | null | undefined
): number | null {
  return parseSentryTimestamp(value)?.getTime() ?? null
}
