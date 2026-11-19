// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  analyticsCheckPassed,
  analyticsCheckWindow,
  buildProjectAnalyticsCountRequest,
  PROJECT_OVERVIEW_ANALYTICS_METRICS,
} from './project-analytics-summary'

describe('buildProjectAnalyticsCountRequest', () => {
  test('does not present raw sessions as human activity', () => {
    expect(PROJECT_OVERVIEW_ANALYTICS_METRICS).not.toContain('sessions')
  })

  test('keeps project overview analytics scoped to the current project', () => {
    const request = buildProjectAnalyticsCountRequest(
      42,
      {
        start_date: '2026-08-16T12:00:00.000Z',
        end_date: '2026-08-17T12:00:00.000Z',
      },
      'visitors'
    )

    expect(request).toEqual({
      path: { project_id: 42 },
      query: {
        start_date: '2026-08-16T12:00:00.000Z',
        end_date: '2026-08-17T12:00:00.000Z',
        metric: 'visitors',
      },
    })
  })
})

describe('the setup check', () => {
  test('a project that already had events needs a new one, not just "has events"', () => {
    const had = { hadEventsBefore: true, hasEvents: true }
    expect(analyticsCheckPassed({ ...had, eventsSinceCheck: undefined })).toBe(
      false
    )
    expect(analyticsCheckPassed({ ...had, eventsSinceCheck: 0 })).toBe(false)
    expect(analyticsCheckPassed({ ...had, eventsSinceCheck: 1 })).toBe(true)
  })

  test('a project with none passes on its first event', () => {
    const none = { hadEventsBefore: false, eventsSinceCheck: undefined }
    expect(analyticsCheckPassed({ ...none, hasEvents: undefined })).toBe(false)
    expect(analyticsCheckPassed({ ...none, hasEvents: false })).toBe(false)
    expect(analyticsCheckPassed({ ...none, hasEvents: true })).toBe(true)
  })

  test('the window starts just before the check and does not move while polling', () => {
    const startedAt = Date.UTC(2026, 0, 2, 12, 0, 0)
    expect(analyticsCheckWindow(startedAt)).toEqual({
      start_date: '2026-01-02T11:59:00.000Z',
      end_date: '2026-01-03T12:00:00.000Z',
    })
  })
})
