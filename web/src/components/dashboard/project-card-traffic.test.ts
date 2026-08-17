import { describe, expect, test } from 'bun:test'
import { projectCardTraffic } from './project-card-traffic'

describe('project card traffic fallback', () => {
  test('prefers visitor activity when analytics has visitors', () => {
    expect(
      projectCardTraffic(
        [{ date: '2026-08-16T10:00:00Z', count: 2 }],
        [{ bucket: '2026-08-16T10:00:00Z', request_count: 80 }],
        80
      )
    ).toEqual({
      kind: 'visitors',
      data: [{ hour: '2026-08-16T10:00:00Z', count: 2 }],
    })
  })

  test('uses real API request buckets when visitor activity is zero', () => {
    expect(
      projectCardTraffic(
        [{ date: '2026-08-16T10:00:00Z', count: 0 }],
        [
          { bucket: '2026-08-16T09:00:00Z', request_count: 12 },
          { bucket: '2026-08-16T10:00:00Z', request_count: 80 },
        ],
        92
      )
    ).toEqual({
      kind: 'api-requests',
      data: [
        { hour: '2026-08-16T09:00:00Z', count: 12 },
        { hour: '2026-08-16T10:00:00Z', count: 80 },
      ],
    })
  })

  test('does not label an empty project as API traffic', () => {
    expect(projectCardTraffic([], [], 0)).toEqual({
      kind: 'visitors',
      data: [],
    })
  })
})
