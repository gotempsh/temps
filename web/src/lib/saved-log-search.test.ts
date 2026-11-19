// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { SavedSearchQuery } from '@/api/client/types.gen'
import {
  savedQueryFromParams,
  savedRangeFromWindow,
  saveSearchRequest,
  viewFromSavedSearch,
  windowFromSavedRange,
} from './saved-log-search'

const now = Date.parse('2026-01-01T12:00:00.000Z')

const emptyQuery: SavedSearchQuery = {
  source: 'collected',
  levels: [],
  envs: [],
  node_ids: [],
  deploy_id: null,
  text: null,
  attrs: [],
  services: [],
  container_ids: [],
  external_service_ids: [],
}

describe('savedRangeFromWindow', () => {
  test('a preset keeps its name under the server spelling', () => {
    expect(savedRangeFromWindow({ preset: '1d', from: '', to: '' })).toBe('24h')
  })

  test('a custom window becomes the nearest preset', () => {
    expect(
      savedRangeFromWindow({
        preset: 'custom',
        from: new Date(now - 5 * 3_600_000).toISOString(),
        to: new Date(now).toISOString(),
      })
    ).toBe('6h')
  })

  test('an empty or reversed window falls back to one hour', () => {
    const at = new Date(now).toISOString()
    expect(savedRangeFromWindow({ preset: 'custom', from: at, to: at })).toBe(
      '1h'
    )
  })
})

describe('windowFromSavedRange', () => {
  test('a range the page has a preset for opens as that preset', () => {
    expect(windowFromSavedRange('7d', now).preset).toBe('7d')
  })

  test('a range without a preset opens as a window ending now', () => {
    const window = windowFromSavedRange('15m', now)
    expect(window.preset).toBe('custom')
    expect(Date.parse(window.to) - Date.parse(window.from)).toBe(900_000)
  })
})

describe('savedQueryFromParams', () => {
  test('reads the page filters', () => {
    const params = new URLSearchParams(
      'source=application&level=ERROR&env=production&node_id=3&deploy_id=9&q=%20payment%20failed%20&attr=status%3D500'
    )
    expect(savedQueryFromParams(params)).toEqual({
      ...emptyQuery,
      source: 'application',
      levels: ['ERROR'],
      envs: ['production'],
      node_ids: [3],
      deploy_id: 9,
      text: 'payment failed',
      attrs: ['status=500'],
    })
  })

  test('ignores values the page would not send', () => {
    const params = new URLSearchParams('source=other&level=LOUD&node_id=-1')
    expect(savedQueryFromParams(params)).toEqual(emptyQuery)
  })
})

describe('viewFromSavedSearch', () => {
  test('opens a search the page can show in full', () => {
    const view = viewFromSavedSearch(
      {
        project_id: 7,
        time_range: '1h',
        query: {
          ...emptyQuery,
          levels: ['WARN'],
          text: 'timeout',
          attrs: ['a=b'],
        },
      },
      now
    )
    expect(view.patch).toMatchObject({
      project_id: '7',
      level: 'WARN',
      q: 'timeout',
      source: undefined,
    })
    expect(view.attrs).toEqual(['a=b'])
    expect(view.window.preset).toBe('1h')
    expect(view.notShown).toEqual([])
  })

  test('names the filters the page has no control for', () => {
    const view = viewFromSavedSearch(
      {
        project_id: 7,
        time_range: '1h',
        query: {
          ...emptyQuery,
          levels: ['WARN', 'ERROR'],
          container_ids: ['abc'],
        },
      },
      now
    )
    expect(view.patch.level).toBe('WARN')
    expect(view.notShown).toEqual(['more than one level', 'containers'])
  })

  test('a database search says the page is wider than the project', () => {
    const view = viewFromSavedSearch(
      { project_id: 17, query: { source: 'service' }, time_range: '1h' },
      Date.parse('2026-01-01T00:00:00Z')
    )
    expect(view.patch.source).toBe('service')
    expect(view.notShown).toContain("this project's databases")
  })
})

describe('saveSearchRequest', () => {
  test('saves what the dialog captured, not what the page shows now', () => {
    const captured = {
      projectId: 7,
      query: { levels: ['ERROR' as const], text: 'payment failed' },
      range: '1h',
    }
    const request = saveSearchRequest(captured, {
      name: 'Payment errors',
      description: '  ',
      pinned: true,
    })
    expect(request.path.project_id).toBe(7)
    expect(request.body.query).toBe(captured.query)
    expect(request.body.time_range).toBe('1h')
    expect(request.body.description).toBeNull()
  })
})
