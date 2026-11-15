// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { SentryEvent, SentryException } from '@/types/sentry'

/**
 * Type guard to check if the data is a Sentry event
 */
export function isSentryEvent(data: unknown): data is SentryEvent {
  if (!data || typeof data !== 'object') {
    return false
  }

  const event = data as any
  return (
    event.source === 'sentry' &&
    event.sentry &&
    typeof event.sentry === 'object' &&
    'event_id' in event.sentry &&
    'platform' in event.sentry
  )
}

/**
 * Extract Sentry event from ErrorEventResponse data field
 */
export function extractSentryEvent(data: unknown): SentryEvent | null {
  if (isSentryEvent(data)) {
    return data
  }
  return null
}

/** Both protocol dictionaries and legacy pair lists remain readable. */
export function sentryPairs(value: unknown): Array<[string, string]> {
  const pairs = Array.isArray(value)
    ? value
    : value && typeof value === 'object'
      ? Object.entries(value)
      : []
  return pairs
    .filter(
      (pair): pair is [string, unknown] =>
        Array.isArray(pair) && typeof pair[0] === 'string' && pair.length === 2
    )
    .map(([key, value]) => [
      key,
      typeof value === 'string'
        ? value
        : (JSON.stringify(value) ?? String(value)),
    ])
}

/** Use native stack normalization without modifying the original source event. */
export function sentryExceptions(event: SentryEvent): SentryException[] {
  const source = Array.isArray(event.sentry.exception)
    ? event.sentry.exception
    : (event.sentry.exception?.values ?? [])
  const fallback = event.stack_trace?.length
    ? { frames: event.stack_trace }
    : event.sentry.stacktrace
  if (source.length === 0 && fallback?.frames.length) {
    const message = event.sentry.message
    return [
      {
        type: 'Message',
        value:
          typeof message === 'string'
            ? message
            : (message?.formatted ??
              event.sentry.logentry?.formatted ??
              'Message'),
        stacktrace: fallback,
      },
    ]
  }
  return source.map((exception, index) =>
    index === source.length - 1 &&
    !exception.stacktrace?.frames.length &&
    fallback
      ? { ...exception, stacktrace: fallback }
      : exception
  )
}
