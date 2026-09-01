// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  resolveCreateEvents,
  selectEventsByIndex,
  parseWebhookEventsList,
  groupEventsByCategory,
  formatDeliveryPayload,
} from './index.js'

const allEventTypes = ['deployment.started', 'deployment.completed', 'domain.verified']

describe('resolveCreateEvents', () => {
  test('resolves "all" (case-insensitive) to every known event type', () => {
    expect(resolveCreateEvents('ALL', allEventTypes)).toEqual({ events: allEventTypes })
  })

  test('splits and trims a comma-separated list of valid events', () => {
    expect(resolveCreateEvents('deployment.started, domain.verified', allEventTypes)).toEqual({
      events: ['deployment.started', 'domain.verified'],
    })
  })

  test('rejects an unknown event type instead of sending a bad subscription', () => {
    expect(resolveCreateEvents('deployment.started,bogus.event', allEventTypes)).toEqual({
      invalidEvent: 'bogus.event',
    })
  })
})

describe('selectEventsByIndex', () => {
  const eventTypes = [
    { event_type: 'deployment.started' },
    { event_type: 'deployment.completed' },
    { event_type: 'domain.verified' },
  ]

  test('converts 1-based comma-separated picks to event type strings', () => {
    expect(selectEventsByIndex('1,3', eventTypes)).toEqual(['deployment.started', 'domain.verified'])
  })

  test('drops out-of-range or non-numeric picks rather than throwing', () => {
    // Interactive input is free text; a typo like "0" or "99" must not crash
    // the prompt, it should just contribute nothing to the selection.
    expect(selectEventsByIndex('0,2,99,abc', eventTypes)).toEqual(['deployment.completed'])
  })
})

describe('parseWebhookEventsList', () => {
  test('resolves "all" to the full type list', () => {
    expect(parseWebhookEventsList('all', allEventTypes)).toEqual(allEventTypes)
  })

  test('splits and trims an explicit list without validating against known types', () => {
    // Unlike resolveCreateEvents (used by `create`), the `update` path does
    // not check membership — this documents that existing asymmetry.
    expect(parseWebhookEventsList('deployment.started, totally.unknown', allEventTypes)).toEqual([
      'deployment.started',
      'totally.unknown',
    ])
  })
})

describe('groupEventsByCategory', () => {
  test('buckets events under their category', () => {
    const events = [
      { event_type: 'a', category: 'Deployments' },
      { event_type: 'b', category: 'Deployments' },
      { event_type: 'c', category: 'Domains' },
    ]
    const grouped = groupEventsByCategory(events)
    expect([...grouped.keys()]).toEqual(['Deployments', 'Domains'])
    expect(grouped.get('Deployments')).toEqual([
      { event_type: 'a', category: 'Deployments' },
      { event_type: 'b', category: 'Deployments' },
    ])
    expect(grouped.get('Domains')).toEqual([{ event_type: 'c', category: 'Domains' }])
  })

  test('falls back to "Other" when category is missing', () => {
    const events = [{ event_type: 'a', category: undefined }]
    const grouped = groupEventsByCategory(events)
    expect(grouped.has('Other')).toBe(true)
  })
})

describe('formatDeliveryPayload', () => {
  test('pretty-prints valid JSON payloads', () => {
    expect(formatDeliveryPayload('{"a":1}')).toBe(JSON.stringify({ a: 1 }, null, 2))
  })

  test('falls back to the raw string when payload is not valid JSON', () => {
    expect(formatDeliveryPayload('not json')).toBe('not json')
  })
})
