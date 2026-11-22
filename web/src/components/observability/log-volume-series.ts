// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { BREAKDOWN_STROKES } from '@/components/charts/chart-colors'

/** Least to most severe: the order levels stack in, bottom to top. */
const LEVEL_ORDER = ['TRACE', 'DEBUG', 'INFO', 'WARN', 'ERROR', 'FATAL']

/** TRACE sits below DEBUG: the same grey, lighter. */
const TRACE_FILL =
  'color-mix(in oklab, var(--muted-foreground) 50%, transparent)'

/**
 * Bar colour per level, in the colours log tools conventionally use: errors
 * red, warnings amber, info blue, debug and trace grey. Errors and warnings
 * match their badges in the list, so a spike of errors reads as one in the
 * chart before any line is opened.
 */
const LEVEL_FILL: Record<string, string> = {
  FATAL: 'var(--destructive)',
  ERROR: 'var(--destructive)',
  WARN: 'var(--warning)',
  INFO: 'var(--chart-1)',
  DEBUG: 'var(--muted-foreground)',
  TRACE: TRACE_FILL,
}

/** Most severe first, for lists where errors should lead. */
export function bySeverityDesc<T extends { value: string }>(values: T[]): T[] {
  const rank = (level: string) => {
    const index = LEVEL_ORDER.indexOf(level.toUpperCase())
    return index === -1 ? -1 : index
  }
  return [...values].sort((a, b) => rank(b.value) - rank(a.value))
}

/** Series in stacking order: by severity for levels, by name otherwise. */
export function orderVolumeGroups(groups: string[], byLevel: boolean) {
  if (!byLevel) return [...groups].sort()
  const rank = (group: string) => {
    const index = LEVEL_ORDER.indexOf(group.toUpperCase())
    return index === -1 ? LEVEL_ORDER.length : index
  }
  return [...groups].sort((a, b) => rank(a) - rank(b) || a.localeCompare(b))
}

export function volumeSeriesFill(
  group: string,
  index: number,
  byLevel: boolean
): string {
  if (byLevel) {
    const fill = LEVEL_FILL[group.toUpperCase()]
    if (fill) return fill
  }
  return BREAKDOWN_STROKES[index % BREAKDOWN_STROKES.length]
}
