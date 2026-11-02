// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { logPaneEmptyMessage, mergedStreamStatus } from './log-stream-status'

test('merged streams report connecting, then connected once any is open', () => {
  expect(mergedStreamStatus(0, 0, 2).status).toBe('connecting')
  expect(mergedStreamStatus(1, 0, 2).status).toBe('connected')
  // One stream failing does not hide the lines of the one still open.
  expect(mergedStreamStatus(1, 50, 2)).toEqual({
    status: 'connected',
    attempt: 0,
  })
})

test('merged streams that never open fail visibly, then stop retrying', () => {
  expect(mergedStreamStatus(0, 1, 2)).toEqual({ status: 'error', attempt: 1 })
  expect(mergedStreamStatus(0, 3, 2)).toEqual({ status: 'error', attempt: 2 })
  expect(mergedStreamStatus(0, 5, 2)).toEqual({ status: 'error', attempt: 3 })
  expect(mergedStreamStatus(0, 6, 2)).toEqual({
    status: 'permanent_error',
    attempt: 3,
  })
})

test('an empty pane says why, unless a connection alert already does', () => {
  const live = {
    mode: 'live',
    status: 'connected',
    totalLines: 0,
    noContainers: false,
    allContainers: false,
  } as const
  expect(logPaneEmptyMessage(live)).toContain('this container')
  expect(logPaneEmptyMessage({ ...live, allContainers: true })).toContain(
    'these containers'
  )
  for (const status of ['connecting', 'error', 'permanent_error'] as const) {
    expect(logPaneEmptyMessage({ ...live, status })).toBeNull()
  }
  expect(logPaneEmptyMessage({ ...live, totalLines: 4 })).toBe(
    'No lines match the selected levels.'
  )
  expect(logPaneEmptyMessage({ ...live, mode: 'pause' })).toContain('Paused')
  expect(logPaneEmptyMessage({ ...live, mode: 'interval' })).toContain('window')
  expect(
    logPaneEmptyMessage({ ...live, status: 'connecting', noContainers: true })
  ).toBe('No containers are running in this environment.')
})
