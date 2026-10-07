// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { OtelMetricAlertRuleResponse } from '@/api/client'
import {
  serializeLabelFilters,
  tuplesToLabelFilters,
} from '@/components/metrics/label-filters'

export type FiringRuleLinks = {
  /** Explore chart of the metric the rule watches, with its filters. */
  metric: string
  /** The alarm the rule raised, or the project's alarms when unknown. */
  alarm: string
  alarmLabel: string
  /** The rule editor. */
  rule: string
}

/**
 * Where a firing metric alert row points: the chart of what fired first,
 * then the alarm it raised (where it can be acknowledged or silenced), then
 * the rule definition.
 *
 * Per-series (dynamic) rules record the alarm each series opened, so the
 * first firing series deep-links to its alarm row. Single-series rules don't
 * expose their alarm id, so they open the project's alarm list instead.
 */
export function firingRuleLinks(
  rule: Pick<
    OtelMetricAlertRuleResponse,
    'id' | 'metric_name' | 'aggregation' | 'label_filters' | 'firing_series'
  >,
  base: string,
  projectId: number
): FiringRuleLinks {
  const params = new URLSearchParams({ metric: rule.metric_name })
  if (rule.aggregation) params.set('agg', rule.aggregation)
  const labels = serializeLabelFilters(tuplesToLabelFilters(rule.label_filters))
  if (labels) params.set('labels', labels)

  const alarmId = (rule.firing_series ?? []).find(
    (series) => series.alarm_id != null
  )?.alarm_id
  const alarmParams = new URLSearchParams({ project_id: String(projectId) })
  if (alarmId != null) alarmParams.set('alarm_id', String(alarmId))

  return {
    metric: `${base}/explore?${params.toString()}`,
    alarm: `/monitoring/alarms?${alarmParams.toString()}`,
    alarmLabel: alarmId != null ? 'View alarm' : 'Alarms',
    rule: `${base}/alerts/${rule.id}/edit`,
  }
}
