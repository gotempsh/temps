// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface LabelFilter {
  key: string
  value: string
}

/** Serialize complete label rows for URL persistence; omit draft blank rows. */
export function serializeLabelFilters(filters: LabelFilter[]): string {
  return filters
    .filter((f) => f.key.trim().length > 0)
    .map((f) => `${f.key.trim()}=${f.value.trim()}`)
    .join(',')
}

export function labelFiltersToTuples(
  filters: LabelFilter[]
): [string, string][] {
  return filters
    .filter((f) => f.key.trim().length > 0)
    .map((f) => [f.key.trim(), f.value.trim()] as [string, string])
}

export function tuplesToLabelFilters(
  tuples: readonly (readonly [string, string])[] | null | undefined
): LabelFilter[] {
  return (tuples ?? []).map(([key, value]) => ({ key, value }))
}
