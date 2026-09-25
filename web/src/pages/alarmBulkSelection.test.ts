// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { AlarmResponse } from '@/api/client/types.gen'
import {
  actionAppliesTo,
  bulkRequestFor,
  canSelectAllMatching,
  EMPTY_SELECTION,
  isRowSelected,
  pageCheckState,
  selectionCount,
  toggleRow,
  togglePage,
} from './alarmBulkSelection'

function alarm(id: number, status: string): AlarmResponse {
  return {
    id,
    project_id: 1,
    alarm_type: 'container_crash',
    severity: 'critical',
    status,
    title: `alarm ${id}`,
    fired_at: '2026-09-25T00:00:00Z',
    created_at: '2026-09-25T00:00:00Z',
    updated_at: '2026-09-25T00:00:00Z',
  } as AlarmResponse
}

const page = [alarm(1, 'firing'), alarm(2, 'acknowledged'), alarm(3, 'resolved')]

describe('page selection', () => {
  test('selecting the page picks only alarms that can still change', () => {
    const selection = togglePage(EMPTY_SELECTION, page)
    expect(selection).toEqual({ kind: 'ids', ids: new Set([1, 2]) })
    expect(pageCheckState(selection, page)).toBe(true)
    expect(isRowSelected(selection, page[2])).toBe(false)
  })

  test('a partial selection is indeterminate and toggling it selects the page', () => {
    const partial = toggleRow(EMPTY_SELECTION, 1)
    expect(pageCheckState(partial, page)).toBe('indeterminate')
    expect(selectionCount(togglePage(partial, page))).toBe(2)
  })

  test('toggling a fully selected page clears it', () => {
    const full = togglePage(EMPTY_SELECTION, page)
    expect(selectionCount(togglePage(full, page))).toBe(0)
  })

  test('a page of only resolved alarms has nothing to select', () => {
    expect(pageCheckState(EMPTY_SELECTION, [alarm(9, 'resolved')])).toBe(false)
  })
})

describe('select all matching', () => {
  test('is offered once the whole page is picked and more alarms exist', () => {
    const full = togglePage(EMPTY_SELECTION, page)
    expect(canSelectAllMatching(full, page, 40)).toBe(true)
    expect(canSelectAllMatching(full, page, page.length)).toBe(false)
    expect(canSelectAllMatching(toggleRow(EMPTY_SELECTION, 1), page, 40)).toBe(
      false
    )
  })

  test('unchecking a row narrows back to explicit rows', () => {
    const narrowed = toggleRow({ kind: 'all-matching' }, 1)
    expect(narrowed.kind).toBe('ids')
    expect(selectionCount({ kind: 'all-matching' })).toBeNull()
  })
})

describe('bulk requests', () => {
  test('explicit rows send alarm_ids', () => {
    const selection = togglePage(EMPTY_SELECTION, page)
    expect(bulkRequestFor('resolve', selection, { status: 'firing' })).toEqual({
      action: 'resolve',
      alarm_ids: [1, 2],
    })
  })

  test('all matching sends the active list filters', () => {
    expect(
      bulkRequestFor(
        'acknowledge',
        { kind: 'all-matching' },
        { severity: 'critical', alarmType: 'container_crash' }
      )
    ).toEqual({
      action: 'acknowledge',
      filter: { severity: 'critical', alarm_type: 'container_crash' },
    })
  })

  test('acknowledge only applies when a firing alarm is selected', () => {
    const ackedOnly = toggleRow(EMPTY_SELECTION, 2)
    expect(actionAppliesTo('acknowledge', ackedOnly, page)).toBe(false)
    expect(actionAppliesTo('resolve', ackedOnly, page)).toBe(true)
    expect(actionAppliesTo('resolve', EMPTY_SELECTION, page)).toBe(false)
  })
})
