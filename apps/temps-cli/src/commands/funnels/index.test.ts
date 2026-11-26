// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { parseStepsJson, stepsToWrite } from './index.js'

describe('parseStepsJson', () => {
  test('parses a valid array of steps', () => {
    const steps = parseStepsJson('[{"event_name":"page_view"},{"event_name":"signup"}]')
    expect(steps).toEqual([{ event_name: 'page_view' }, { event_name: 'signup' }])
  })

  test('rejects unparsable JSON instead of sending a broken request', () => {
    expect(parseStepsJson('not json')).toBeNull()
  })

  test('rejects a JSON object that is not an array', () => {
    // The most likely mistake: passing a single step object instead of an
    // array of steps. Sending it through would corrupt the funnel creation
    // request rather than failing loudly here.
    expect(parseStepsJson('{"event_name":"page_view"}')).toBeNull()
  })

  test('rejects steps missing an event_name', () => {
    expect(parseStepsJson('[{"foo":"bar"}]')).toBeNull()
  })

  test('rejects steps whose event_name is not a string', () => {
    expect(parseStepsJson('[{"event_name":123}]')).toBeNull()
  })

  test('accepts an empty array', () => {
    expect(parseStepsJson('[]')).toEqual([])
  })
})

describe('stepsToWrite', () => {
  test('keeps the stored steps, in their order, when an update leaves them out', () => {
    const steps = stepsToWrite([
      { id: 12, order: 2, event_name: 'signup', event_filter: [] },
      {
        id: 11,
        order: 1,
        event_name: 'page_view',
        event_filter: [{ type: 'page_path', value: '/pricing' }],
      },
    ])
    expect(steps).toEqual([
      { event_name: 'page_view', event_filter: [{ type: 'page_path', value: '/pricing' }] },
      { event_name: 'signup', event_filter: [] },
    ])
  })
})
