import { SentryEvent } from '@/types/sentry'

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
