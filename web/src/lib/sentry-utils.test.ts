// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { describe, expect, test } from 'bun:test'
import type { SentryEvent } from '@/types/sentry'
import { sentryExceptions, sentryPairs } from './sentry-utils'

describe('Sentry source presentation', () => {
  test('dictionary and legacy headers/tags display without dropping typed values', () => {
    expect(
      sentryPairs({
        'User-Agent': 'SyntheticAgent',
        nullable: null,
        array: [1, true],
      })
    ).toEqual([
      ['User-Agent', 'SyntheticAgent'],
      ['nullable', 'null'],
      ['array', '[1,true]'],
    ])
    expect(sentryPairs([['key', 'value'], ['bad'], null, [42, 'bad']])).toEqual(
      [['key', 'value']]
    )
    expect(sentryPairs(undefined)).toEqual([])
  })
  test('a native thread stack belongs to the primary exception and leaves source unchanged', () => {
    const event = {
      source: 'sentry',
      stack_trace: [{ filename: 'synthetic.rs', function: 'run' }],
      sentry: {
        exception: {
          values: [
            { type: 'Inner', value: 'inner' },
            { type: 'Outer', value: 'outer' },
          ],
        },
      },
    } as SentryEvent
    const before = JSON.stringify(event)
    const exceptions = sentryExceptions(event)
    expect(exceptions[0].stacktrace).toBeUndefined()
    expect(exceptions[1].stacktrace?.frames).toEqual(event.stack_trace)
    expect(JSON.stringify(event)).toBe(before)
    event.sentry.exception = [
      { type: 'SyntheticGoError', value: 'Synthetic Go failure' },
    ]
    expect(sentryExceptions(event)[0].type).toBe('SyntheticGoError')
    event.sentry.exception = undefined
    event.sentry.message = 'Synthetic message'
    expect(sentryExceptions(event)[0].value).toBe('Synthetic message')
    expect(sentryExceptions(event)[0].stacktrace?.frames).toEqual(
      event.stack_trace
    )
  })
})
