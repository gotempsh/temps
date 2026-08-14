import { describe, expect, test } from 'bun:test'
import { parseStructuredSseFrame } from '@/hooks/useStructuredAi'
import { apiTrafficSummaryCacheKey } from './ApiTraffic'
import { shouldRequestApiTrafficSummary } from '@/lib/ai-summary'

describe('structured AI SSE parsing', () => {
  test('parses a partial JSON event', () => {
    expect(
      parseStructuredSseFrame(
        'event: partial\r\ndata: {"type":"partial","value":{"headline":"Ready"}}'
      )
    ).toEqual({ type: 'partial', value: { headline: 'Ready' } })
  })

  test('ignores keep-alive frames without data', () => {
    expect(parseStructuredSseFrame(': keep-alive')).toBeNull()
  })

  test('parses resolved provider, model, and thinking metadata', () => {
    expect(
      parseStructuredSseFrame(
        'event: started\ndata: {"type":"started","cached":false,"provider":"openai","model":"gpt-5.6","thinking_level":"high"}'
      )
    ).toEqual({
      type: 'started',
      cached: false,
      provider: 'openai',
      model: 'gpt-5.6',
      thinking_level: 'high',
    })
  })
})

describe('API traffic summary cache identity', () => {
  test('keeps rolling filters stable when their timestamps move', () => {
    const first = apiTrafficSummaryCacheKey({
      filter: '24hours',
      startIso: '2026-08-13T15:00:00Z',
      endIso: '2026-08-14T15:00:00Z',
    })
    const later = apiTrafficSummaryCacheKey({
      filter: '24hours',
      startIso: '2026-08-13T15:00:40Z',
      endIso: '2026-08-14T15:00:40Z',
    })

    expect(later).toBe(first)
  })

  test('keeps custom windows distinct', () => {
    const first = apiTrafficSummaryCacheKey({
      filter: 'custom',
      startIso: '2026-08-13T15:00:00Z',
      endIso: '2026-08-14T15:00:00Z',
    })
    const later = apiTrafficSummaryCacheKey({
      filter: 'custom',
      startIso: '2026-08-13T15:00:40Z',
      endIso: '2026-08-14T15:00:40Z',
    })

    expect(later).not.toBe(first)
  })
})

describe('API traffic summary request gating', () => {
  test('never spends AI credits before an explicit request', () => {
    expect(
      shouldRequestApiTrafficSummary({
        requested: false,
        dataReady: true,
        consentEnabled: true,
      })
    ).toBe(false)
  })

  test('requires consent and loaded analytics after the user requests it', () => {
    expect(
      shouldRequestApiTrafficSummary({
        requested: true,
        dataReady: false,
        consentEnabled: true,
      })
    ).toBe(false)
    expect(
      shouldRequestApiTrafficSummary({
        requested: true,
        dataReady: true,
        consentEnabled: false,
      })
    ).toBe(false)
    expect(
      shouldRequestApiTrafficSummary({
        requested: true,
        dataReady: true,
        consentEnabled: true,
      })
    ).toBe(true)
  })
})
