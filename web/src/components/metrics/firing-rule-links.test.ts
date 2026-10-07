// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { firingRuleLinks } from './firing-rule-links'

const base = '/projects/app/metrics'

describe('firingRuleLinks', () => {
  test('links the metric chart, the alarm it raised, and the rule', () => {
    const links = firingRuleLinks(
      {
        id: 5,
        metric_name: 'http.server.duration',
        aggregation: 'p95',
        label_filters: [['route', '/api']],
        firing_series: [
          { series_key: [], series_label: 'a', alarm_id: null },
          { series_key: [], series_label: 'b', alarm_id: 42 },
        ],
      },
      base,
      7
    )
    expect(links).toEqual({
      metric:
        '/projects/app/metrics/explore?metric=http.server.duration&agg=p95&labels=route%3D%2Fapi',
      alarm: '/monitoring/alarms?project_id=7&alarm_id=42',
      alarmLabel: 'View alarm',
      rule: '/projects/app/metrics/alerts/5/edit',
    })
  })

  test('falls back to the project alarm list when no alarm id is known', () => {
    const links = firingRuleLinks(
      { id: 5, metric_name: 'm', aggregation: '', label_filters: [] },
      base,
      7
    )
    expect(links.metric).toBe('/projects/app/metrics/explore?metric=m')
    expect(links.alarm).toBe('/monitoring/alarms?project_id=7')
    expect(links.alarmLabel).toBe('Alarms')
  })
})
