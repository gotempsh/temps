// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  buildGoalInput,
  describeGoal,
  formatRevenue,
  parseDimension,
  parseGoalId,
} from './conversions'

describe('buildGoalInput', () => {
  test('an event conversion with a property filter and revenue', () => {
    expect(
      buildGoalInput({
        name: 'Pro purchase',
        event: 'purchase',
        property: 'plan=pro',
        revenueProperty: 'amount',
        currency: 'eur',
      }),
    ).toEqual({
      ok: true,
      body: {
        name: 'Pro purchase',
        kind: 'event',
        event_name: 'purchase',
        page_path: null,
        event_filter: [{ type: 'custom_data', value: { path: 'plan', value: 'pro' } }],
        revenue_property: 'amount',
        revenue_currency: 'EUR',
      },
    })
  })

  test('a page conversion', () => {
    const result = buildGoalInput({ name: 'Docs', page: '/docs/*' })
    expect(result.ok && result.body.kind).toBe('page')
    expect(result.ok && result.body.page_path).toBe('/docs/*')
  })

  test('refuses ambiguous or incomplete definitions', () => {
    const error = (flags: Parameters<typeof buildGoalInput>[0]) => {
      const result = buildGoalInput(flags)
      return result.ok ? null : result.error
    }
    expect(error({ event: 'signup' })).toContain('--name')
    expect(error({ name: 'x' })).toContain('exactly one')
    expect(error({ name: 'x', event: 'a', page: '/b' })).toContain('exactly one')
    expect(error({ name: 'x', page: 'thanks' })).toContain('must start with /')
    expect(error({ name: 'x', page: '/t', property: 'a=b' })).toContain('only apply to --event')
    expect(error({ name: 'x', event: 'a', currency: 'EUR' })).toContain('go together')
    expect(error({ name: 'x', event: 'a', property: 'plan' })).toContain('key=value')
    expect(error({ name: 'x', event: 'a', property: '=pro' })).toContain('key=value')
  })
})

describe('argument parsing', () => {
  test('goal ids are positive integers', () => {
    expect(parseGoalId('3')).toBe(3)
    expect(() => parseGoalId('0')).toThrow('--goal-id')
    expect(() => parseGoalId('x')).toThrow('--goal-id')
  })

  test('dimensions accept dashes for underscores', () => {
    expect(parseDimension('utm-source')).toBe('utm_source')
    expect(() => parseDimension('city')).toThrow('--by must be one of')
  })
})

describe('labels', () => {
  test('describes goals and revenue', () => {
    const goal = {
      id: 1,
      project_id: 1,
      name: 'Signup',
      kind: 'event' as const,
      event_name: 'signup',
      event_filter: [{ type: 'custom_data' as const, value: { path: 'plan', value: 'pro' } }],
      page_path: null,
      revenue_property: null,
      revenue_currency: null,
      created_at: '2026-11-19T00:00:00Z',
      updated_at: '2026-11-19T00:00:00Z',
    }
    expect(describeGoal(goal)).toBe('event signup where plan=pro')
    expect(describeGoal({ ...goal, kind: 'page', page_path: '/t', event_filter: [] })).toBe(
      'page /t',
    )
    expect(formatRevenue({ currency: 'EUR', total: 12.5, average: 6.25 })).toBe('12.50 EUR')
    expect(formatRevenue(null)).toBe('-')
  })
})
