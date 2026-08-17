import { describe, expect, test } from 'bun:test'
import {
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
