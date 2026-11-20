// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { AnalyticsGoal } from '@/api/client/types.gen'
import {
  EMPTY_CONVERSION_FORM,
  conversionFormSchema,
  describeGoal,
  formValuesToInput,
  formatRate,
  formatRevenue,
  goalToFormValues,
} from './conversions'

function goal(overrides: Partial<AnalyticsGoal> = {}): AnalyticsGoal {
  return {
    id: 3,
    project_id: 1,
    name: 'Purchase',
    kind: 'event',
    event_name: 'purchase',
    event_filter: [],
    page_path: null,
    revenue_property: null,
    revenue_currency: null,
    created_at: '2026-11-19T00:00:00Z',
    updated_at: '2026-11-19T00:00:00Z',
    ...overrides,
  }
}

function issues(values: Partial<typeof EMPTY_CONVERSION_FORM>): string[] {
  const result = conversionFormSchema.safeParse({
    ...EMPTY_CONVERSION_FORM,
    name: 'Goal',
    ...values,
  })
  return result.success
    ? []
    : result.error.issues.map((issue) => issue.path.join('.'))
}

describe('conversion form', () => {
  test('an event conversion needs an event name', () => {
    expect(issues({ kind: 'event' })).toEqual(['eventName'])
    expect(issues({ kind: 'event', eventName: 'signup' })).toEqual([])
  })

  test('a page conversion needs a path starting with /', () => {
    expect(issues({ kind: 'page', pagePath: 'thanks' })).toEqual(['pagePath'])
    expect(issues({ kind: 'page', pagePath: '/thanks' })).toEqual([])
  })

  test('a property filter needs a safe key and a value', () => {
    expect(issues({ eventName: 'signup', propertyKey: 'plan-tier' })).toEqual([
      'propertyKey',
      'propertyValue',
    ])
    expect(
      issues({
        eventName: 'signup',
        propertyKey: 'plan.tier',
        propertyValue: 'pro',
      })
    ).toEqual([])
  })

  test('revenue needs a property and a three-letter currency', () => {
    expect(issues({ eventName: 'buy', revenueCurrency: 'EUR' })).toEqual([
      'revenueProperty',
    ])
    expect(
      issues({
        eventName: 'buy',
        revenueProperty: 'amount',
        revenueCurrency: 'EURO',
      })
    ).toEqual(['revenueCurrency'])
  })
})

describe('form values and goals', () => {
  test('a page conversion sends no event fields', () => {
    expect(
      formValuesToInput({
        ...EMPTY_CONVERSION_FORM,
        name: 'Thanks',
        kind: 'page',
        pagePath: '/thank-you',
        eventName: 'left over',
        revenueProperty: 'amount',
        revenueCurrency: 'eur',
      })
    ).toEqual({
      name: 'Thanks',
      kind: 'page',
      event_name: null,
      page_path: '/thank-you',
      event_filter: [],
      revenue_property: null,
      revenue_currency: null,
    })
  })

  test('an event conversion sends its property filter and upper-cased currency', () => {
    const input = formValuesToInput({
      ...EMPTY_CONVERSION_FORM,
      name: 'Pro purchase',
      eventName: 'purchase',
      propertyKey: 'plan',
      propertyValue: 'pro',
      revenueProperty: 'amount',
      revenueCurrency: 'eur',
    })
    expect(input.event_filter).toEqual([
      { type: 'custom_data', value: { path: 'plan', value: 'pro' } },
    ])
    expect(input.revenue_currency).toBe('EUR')
  })

  test('editing keeps filters the form does not show', () => {
    const existing = goal({
      event_filter: [
        { type: 'utm_source', value: 'newsletter' },
        { type: 'custom_data', value: { path: 'plan', value: 'pro' } },
      ],
    })
    const values = goalToFormValues(existing)
    expect(values.propertyKey).toBe('plan')
    const input = formValuesToInput(
      { ...values, propertyKey: 'tier', propertyValue: 'gold' },
      existing
    )
    expect(input.event_filter).toEqual([
      { type: 'utm_source', value: 'newsletter' },
      { type: 'custom_data', value: { path: 'tier', value: 'gold' } },
    ])
  })
})

describe('labels', () => {
  test('describes what a goal matches', () => {
    expect(describeGoal(goal())).toBe('Event purchase')
    expect(
      describeGoal(
        goal({
          event_filter: [
            { type: 'custom_data', value: { path: 'plan', value: 'pro' } },
          ],
        })
      )
    ).toBe('Event purchase where plan = pro')
    expect(
      describeGoal(
        goal({ kind: 'page', event_name: null, page_path: '/docs/*' })
      )
    ).toBe('Visit to /docs/*')
  })

  test('formats rates and money', () => {
    expect(formatRate(0)).toBe('0%')
    expect(formatRate(2.345)).toBe('2.3%')
    expect(formatRate(66.67)).toBe('67%')
    expect(formatRevenue(50, 'USD')).toBe('$50.00')
    expect(formatRevenue(5, 'ZZZZ')).toBe('5.00 ZZZZ')
  })

  test('renaming a page goal keeps its property filter', () => {
    const existing = goal({
      kind: 'page',
      event_name: null,
      page_path: '/thank-you',
      event_filter: [
        { type: 'custom_data', value: { path: 'plan', value: 'pro' } },
      ],
    })
    const input = formValuesToInput(
      { ...goalToFormValues(existing), name: 'Renamed' },
      existing
    )
    expect(input.event_filter).toEqual([
      { type: 'custom_data', value: { path: 'plan', value: 'pro' } },
    ])
  })

  test('turning an event goal into a page goal drops its event filters', () => {
    const existing = goal({
      kind: 'event',
      event_name: 'purchase',
      page_path: null,
      event_filter: [
        { type: 'custom_data', value: { path: 'plan', value: 'pro' } },
        { type: 'channel', value: 'Email' },
      ],
    })
    const input = formValuesToInput(
      { ...goalToFormValues(existing), kind: 'page', pagePath: '/thank-you' },
      existing
    )
    expect(input).toMatchObject({
      kind: 'page',
      page_path: '/thank-you',
      event_name: null,
      event_filter: [],
    })
  })
})
