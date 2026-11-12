// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Data key of the dashed segment into a still-accumulating last point. */
export const PARTIAL_KEY = '__partial'

/**
 * Rows for a chart whose last point is partial: the solid series stops one
 * point early and `PARTIAL_KEY` carries the last two values, so the line ends
 * in a dashed segment rather than plunging. `data` is left untouched.
 */
export function withPartialLastPoint<T extends Record<string, unknown>>(
  data: T[],
  dataKey: string
): Array<T & { [PARTIAL_KEY]?: unknown }> {
  if (data.length < 2) return data
  const last = data.length - 1
  return data.map((point, index) => {
    if (index < last - 1) return point
    if (index === last - 1) return { ...point, [PARTIAL_KEY]: point[dataKey] }
    return { ...point, [dataKey]: null, [PARTIAL_KEY]: point[dataKey] }
  })
}

/**
 * One tooltip entry per point: the partial segment's entry is dropped where
 * the solid series has the value, and the solid entry where it has none.
 */
export function partialTooltipEntries<
  E extends { dataKey?: unknown; value?: unknown },
>(entries: readonly E[] | undefined): E[] {
  const list = entries ?? []
  const solid = list.some((e) => e.dataKey !== PARTIAL_KEY && e.value != null)
  return list.filter((e) =>
    e.dataKey === PARTIAL_KEY ? !solid : e.value != null
  )
}
