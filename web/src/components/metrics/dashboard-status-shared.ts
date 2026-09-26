// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { OtelDashboardResponse } from '@/api/client'

export type Section = NonNullable<
  OtelDashboardResponse['layout']
>['sections'][number]

/** Flatten a dashboard's sections to the (metric, aggregation) pairs it plots. */
export function dashboardTiles(
  sections: Section[] | undefined
): { metricName: string; aggregation?: string }[] {
  return (sections ?? []).flatMap((s) =>
    (s.tiles ?? []).map((t) => ({
      metricName: t.metric_name,
      aggregation: t.aggregation,
    }))
  )
}
