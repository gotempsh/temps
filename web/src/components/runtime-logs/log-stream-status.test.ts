// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  logPaneEmptyMessage,
  MAX_STREAM_ATTEMPTS,
  mergedStreamStatus,
  streamGaveUp,
  type MergedStream,
} from './log-stream-status'

const stream = (overrides: Partial<MergedStream> = {}): MergedStream => ({
  open: false,
  failures: 0,
  ended: false,
  ...overrides,
})

test('merged streams report connecting, then connected once any is open', () => {
  expect(mergedStreamStatus([stream(), stream()]).status).toBe('connecting')
  expect(mergedStreamStatus([stream({ open: true }), stream()]).status).toBe(
    'connected'
  )
})

test('one failing stream does not hide the lines of the one still open', () => {
  const failing = stream({ failures: MAX_STREAM_ATTEMPTS })
  expect(mergedStreamStatus([stream({ open: true }), failing])).toEqual({
    status: 'connected',
    attempt: 0,
  })
})

test('each stream gives up on its own failures, whatever the others are doing', () => {
  expect(streamGaveUp(stream({ failures: MAX_STREAM_ATTEMPTS - 1 }))).toBe(
    false
  )
  expect(streamGaveUp(stream({ failures: MAX_STREAM_ATTEMPTS }))).toBe(true)
})

test('streams that never open fail visibly, then stop once all have given up', () => {
  expect(mergedStreamStatus([stream({ failures: 1 }), stream()])).toEqual({
    status: 'error',
    attempt: 1,
  })
  // One has given up, the other is still trying: not final yet.
  expect(
    mergedStreamStatus([stream({ failures: 3 }), stream({ failures: 1 })])
  ).toEqual({ status: 'error', attempt: 3 })
  expect(
    mergedStreamStatus([stream({ failures: 3 }), stream({ failures: 3 })])
  ).toEqual({ status: 'permanent_error', attempt: 3 })
})

test('streams the server closed normally end, rather than staying connected', () => {
  expect(
    mergedStreamStatus([stream({ ended: true }), stream({ ended: true })])
  ).toEqual({ status: 'ended', attempt: 0 })
  // Still one to wait for.
  expect(mergedStreamStatus([stream({ ended: true }), stream()]).status).toBe(
    'connecting'
  )
  // An end does not excuse the stream that could not connect.
  expect(
    mergedStreamStatus([stream({ ended: true }), stream({ failures: 3 })])
      .status
  ).toBe('permanent_error')
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
  for (const status of [
    'connecting',
    'error',
    'permanent_error',
    'ended',
  ] as const) {
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
