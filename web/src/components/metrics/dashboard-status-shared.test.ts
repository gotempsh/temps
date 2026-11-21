// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { dashboardTiles, type Section } from './dashboard-status-shared'

describe('dashboardTiles', () => {
  test('leaves out tiles that chart a saved log search', () => {
    const sections: Section[] = [
      {
        id: 's1',
        title: 'Overview',
        tiles: [
          { id: 't1', metric_name: 'http.server.duration', aggregation: 'p95' },
          {
            id: 't2',
            metric_name: '',
            aggregation: 'count',
            source: { kind: 'logs', saved_search_id: 4, metric: 'count' },
          },
        ],
      },
    ]
    expect(dashboardTiles(sections)).toEqual([
      { metricName: 'http.server.duration', aggregation: 'p95' },
    ])
  })
})
