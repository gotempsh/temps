// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  AlarmResponse,
  BulkAlarmActionRequest,
  BulkAlarmFilter,
  BulkAlarmRequest,
} from '@/api/client/types.gen'

/**
 * What the Alarms page has selected for a bulk action: specific rows on the
 * current page, or every alarm matching the active filters (which can span
 * many pages).
 */
export type AlarmSelection =
  | { kind: 'ids'; ids: ReadonlySet<number> }
  | { kind: 'all-matching' }

export const EMPTY_SELECTION: AlarmSelection = { kind: 'ids', ids: new Set() }

/** Resolved alarms have nothing left to acknowledge or resolve. */
export function isSelectable(alarm: AlarmResponse): boolean {
  return alarm.status !== 'resolved'
}

export function selectableIds(items: readonly AlarmResponse[]): number[] {
  return items.filter(isSelectable).map((alarm) => alarm.id)
}

/**
 * Number of explicitly selected alarms, or `null` for "all matching" — the
 * list total also counts resolved alarms, so the exact number is only known
 * once the server applies the action.
 */
export function selectionCount(selection: AlarmSelection): number | null {
  return selection.kind === 'ids' ? selection.ids.size : null
}

export function isRowSelected(selection: AlarmSelection, alarm: AlarmResponse): boolean {
  if (!isSelectable(alarm)) return false
  return selection.kind === 'all-matching' || selection.ids.has(alarm.id)
}

export function toggleRow(selection: AlarmSelection, alarmId: number): AlarmSelection {
  // Unchecking a row while "all matching" is active narrows back to the
  // explicit rows the user can see, rather than silently keeping the filter.
  const ids = new Set(selection.kind === 'ids' ? selection.ids : [])
  if (ids.has(alarmId)) ids.delete(alarmId)
  else ids.add(alarmId)
  return { kind: 'ids', ids }
}

/** Header checkbox state for the visible page. */
export function pageCheckState(
  selection: AlarmSelection,
  items: readonly AlarmResponse[]
): boolean | 'indeterminate' {
  const ids = selectableIds(items)
  if (ids.length === 0) return false
  if (selection.kind === 'all-matching') return true
  const picked = ids.filter((id) => selection.ids.has(id)).length
  if (picked === 0) return false
  return picked === ids.length ? true : 'indeterminate'
}

export function togglePage(
  selection: AlarmSelection,
  items: readonly AlarmResponse[]
): AlarmSelection {
  if (pageCheckState(selection, items) === true) return EMPTY_SELECTION
  return { kind: 'ids', ids: new Set(selectableIds(items)) }
}

/**
 * Offer "select all N matching" only once every selectable row on this page
 * is picked and more active alarms exist beyond it.
 */
export function canSelectAllMatching(
  selection: AlarmSelection,
  items: readonly AlarmResponse[],
  totalMatching: number
): boolean {
  return (
    selection.kind === 'ids' &&
    pageCheckState(selection, items) === true &&
    totalMatching > items.length
  )
}

export interface AlarmListFilters {
  status?: string
  severity?: string
  alarmType?: string
}

/** Request body for the selection, using the same filters as the list. */
export function bulkRequestFor(
  action: BulkAlarmActionRequest,
  selection: AlarmSelection,
  filters: AlarmListFilters
): BulkAlarmRequest {
  if (selection.kind === 'ids') {
    return { action, alarm_ids: [...selection.ids] }
  }
  const filter: BulkAlarmFilter = {}
  if (filters.status) filter.status = filters.status
  if (filters.severity) filter.severity = filters.severity
  if (filters.alarmType) filter.alarm_type = filters.alarmType
  return { action, filter }
}

/** Whether the action can change anything in the current selection. */
export function actionAppliesTo(
  action: BulkAlarmActionRequest,
  selection: AlarmSelection,
  items: readonly AlarmResponse[]
): boolean {
  if (selection.kind === 'all-matching') return true
  if (selection.ids.size === 0) return false
  if (action === 'resolve') return true
  // Only firing alarms can be acknowledged.
  return items.some(
    (alarm) => selection.ids.has(alarm.id) && alarm.status === 'firing'
  )
}
