// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Which columns the variables table has room for; the rest stack under the key. */
export interface VariableTableLayout {
  valueColumn: boolean
  environmentsColumn: boolean
}

/** Below this a key column squeezed by the badges wraps on every row. */
const ENVIRONMENTS_COLUMN_MIN_WIDTH = 680
/** Below this the key and its masked value cannot share a line. */
const VALUE_COLUMN_MIN_WIDTH = 520

/** An unmeasured or zero width (not laid out yet) keeps every column. */
export function variableTableLayout(width: number | null): VariableTableLayout {
  if (!width) return { valueColumn: true, environmentsColumn: true }
  return {
    valueColumn: width >= VALUE_COLUMN_MIN_WIDTH,
    environmentsColumn: width >= ENVIRONMENTS_COLUMN_MIN_WIDTH,
  }
}

/** Selection, key, checks and actions, plus whichever columns have room. */
export function variableTableColumnCount(layout: VariableTableLayout) {
  return 4 + Number(layout.valueColumn) + Number(layout.environmentsColumn)
}
