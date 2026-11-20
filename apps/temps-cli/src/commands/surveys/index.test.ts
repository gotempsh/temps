// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  assertStatus,
  describeQuestionResults,
  describeTargeting,
  formatAnswer,
  parseSurveyId,
  projectLookup,
  readQuestions,
  readTargeting,
  windowFromDays,
} from './index.js'

describe('surveys command helpers', () => {
  test('--project takes a numeric id or a slug', () => {
    expect(projectLookup('42')).toEqual({ id: 42 })
    expect(projectLookup('shop-2')).toEqual({ slug: 'shop-2' })
  })

  test('survey ids are positive whole numbers', () => {
    expect(parseSurveyId('12')).toBe(12)
    expect(() => parseSurveyId('0')).toThrow(/positive whole number/)
    expect(() => parseSurveyId('nps')).toThrow(/"nps"/)
  })

  test('statuses are checked before any request', () => {
    expect(assertStatus('paused')).toBe('paused')
    expect(() => assertStatus('live')).toThrow(/draft, active, paused, completed/)
  })

  test('--days becomes a window ending now', () => {
    const now = new Date('2026-11-20T12:00:00Z')
    expect(windowFromDays('7', now)).toEqual({
      start_date: '2026-11-13T12:00:00.000Z',
      end_date: '2026-11-20T12:00:00.000Z',
    })
    expect(windowFromDays(undefined, now)).toBeUndefined()
    expect(() => windowFromDays('400', now)).toThrow(/1 to 366/)
    expect(() => windowFromDays('1.5', now)).toThrow(/whole number/)
  })

  test('questions must be an array and targeting an object', async () => {
    expect(await readQuestions('[{"id":"q1","kind":"nps","prompt":"Recommend us?"}]')).toHaveLength(1)
    await expect(readQuestions('{"id":"q1"}')).rejects.toThrow(/JSON array/)
    expect(await readTargeting('{"sample_rate":0.5}')).toEqual({ sample_rate: 0.5 })
    await expect(readTargeting('[]')).rejects.toThrow(/JSON object/)
  })

  test('describes when a survey shows', () => {
    expect(describeTargeting({})).toBe('on every page, once per visitor')
    expect(
      describeTargeting({
        path_pattern: '/checkout/*',
        trigger_event: 'checkout_completed',
        sample_rate: 0.25,
        once_per_visitor: false,
        flag: { key: 'beta', value: true },
      }),
    ).toBe('on /checkout/*, after "checkout_completed", 25% of visitors, where beta = true')
  })

  test('summarises one question', () => {
    expect(
      describeQuestionResults({
        question_id: 'q1',
        kind: 'nps',
        prompt: 'Recommend us?',
        answered: 4,
        distribution: [
          { value: '10', count: 3 },
          { value: '2', count: 1 },
        ],
        other: 0,
        average: 8,
        nps: { promoters: 3, passives: 0, detractors: 1, score: 50 },
      }),
    ).toEqual([
      '4 answered, average 8.0',
      'NPS 50 (3 promoters, 0 passives, 1 detractors)',
      '10: 3 (75%)',
      '2: 1 (25%)',
    ])
    expect(formatAnswer(['a', 'b'])).toBe('a, b')
  })
})
