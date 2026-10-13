// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  deriveJobLogView,
  snapshotTailLines,
  missingLogLines,
  FALLBACK_POLL_INTERVAL_MS,
  type JobLogEntry,
  jobLogPhase,
  type JobLogViewInput,
  MAX_VIEWER_LINES,
  mergeLogEntries,
  parseJobLogContent,
  parseStreamMessage,
  reconnectDelayMs,
  shouldReadSnapshot,
  snapshotPollInterval,
  WAITING_POLL_INTERVAL_MS,
} from './job-log-state'

const NOW = '2026-01-01T00:00:00.000Z'
const now = () => NOW

function entry(line: number, message = `line ${line}`): JobLogEntry {
  return { level: 'info', message, timestamp: NOW, line }
}

function jsonl(...lines: JobLogEntry[]): string {
  return lines.map((line) => JSON.stringify(line)).join('\n') + '\n'
}

function view(
  overrides: Partial<JobLogViewInput>
): ReturnType<typeof deriveJobLogView> {
  return deriveJobLogView({
    phase: 'finished',
    socketState: 'idle',
    entryCount: 0,
    snapshot: { data: undefined, isPending: true, isError: false },
    ...overrides,
  })
}

describe('jobLogPhase', () => {
  test('maps job statuses to log phases', () => {
    expect(jobLogPhase('pending')).toBe('waiting')
    expect(jobLogPhase('waiting')).toBe('waiting')
    expect(jobLogPhase('running')).toBe('live')
    for (const status of ['success', 'failure', 'cancelled', 'skipped']) {
      expect(jobLogPhase(status)).toBe('finished')
    }
  })

  test('treats an unknown status as finished so it is read once over HTTP', () => {
    expect(jobLogPhase('something-new')).toBe('finished')
  })
})

describe('transport selection', () => {
  test('finished and waiting jobs read over HTTP without a socket', () => {
    expect(shouldReadSnapshot('finished', 'idle')).toBe(true)
    expect(shouldReadSnapshot('waiting', 'idle')).toBe(true)
  })

  test('a running job reads over HTTP only while the socket is down', () => {
    expect(shouldReadSnapshot('live', 'idle')).toBe(false)
    expect(shouldReadSnapshot('live', 'connecting')).toBe(false)
    expect(shouldReadSnapshot('live', 'open')).toBe(false)
    expect(shouldReadSnapshot('live', 'failed')).toBe(true)
  })

  test('polls a not-started job and a running job whose socket failed', () => {
    expect(snapshotPollInterval('waiting', 'idle', undefined)).toBe(
      WAITING_POLL_INTERVAL_MS
    )
    expect(snapshotPollInterval('live', 'failed', undefined)).toBe(
      FALLBACK_POLL_INTERVAL_MS
    )
  })

  test('never polls a finished job, a healthy socket, or a gone log', () => {
    expect(snapshotPollInterval('finished', 'idle', undefined)).toBe(false)
    expect(snapshotPollInterval('live', 'open', undefined)).toBe(false)
    expect(
      snapshotPollInterval('waiting', 'idle', { kind: 'gone', detail: 'x' })
    ).toBe(false)
  })

  test('reconnect backoff doubles and is capped', () => {
    expect(reconnectDelayMs(0)).toBe(1_000)
    expect(reconnectDelayMs(1)).toBe(2_000)
    expect(reconnectDelayMs(3)).toBe(8_000)
    expect(reconnectDelayMs(10)).toBe(15_000)
  })
})

describe('parseJobLogContent', () => {
  test('parses the JSONL body of the HTTP read', () => {
    const entries = parseJobLogContent(
      jsonl(entry(1, 'Cloning\n'), {
        ...entry(2, 'Build failed'),
        level: 'error',
      }),
      now
    )
    expect(entries).toEqual([
      entry(1, 'Cloning'),
      { ...entry(2, 'Build failed'), level: 'error' },
    ])
  })

  test('an empty body (job not started) yields no entries', () => {
    expect(parseJobLogContent('', now)).toEqual([])
    expect(parseJobLogContent('\n\n', now)).toEqual([])
  })

  test('numbers plain-text lines after the last known line', () => {
    const entries = parseJobLogContent(
      `${JSON.stringify(entry(4))}\nraw output\n`,
      now
    )
    expect(entries.map((e) => [e.line, e.message])).toEqual([
      [4, 'line 4'],
      [5, 'raw output'],
    ])
  })
})

describe('parseStreamMessage', () => {
  test('passes log entries through', () => {
    expect(parseStreamMessage(JSON.stringify(entry(7)), 6, now)).toEqual({
      kind: 'entry',
      entry: entry(7),
    })
  })

  test('recognises the tail handler error frame', () => {
    expect(
      parseStreamMessage(
        JSON.stringify({ error: 'Failed to tail job logs', detail: 'gone' }),
        0,
        now
      )
    ).toEqual({ kind: 'error', message: 'Failed to tail job logs: gone' })
    expect(parseStreamMessage('ERROR: read failed', 0, now)).toEqual({
      kind: 'error',
      message: 'read failed',
    })
  })

  test('numbers plain-text frames after the last seen line', () => {
    expect(parseStreamMessage('hello', 9, now)).toEqual({
      kind: 'entry',
      entry: { level: 'info', message: 'hello', timestamp: NOW, line: 10 },
    })
  })
})

describe('mergeLogEntries', () => {
  test('appends strictly newer lines', () => {
    const merged = mergeLogEntries([entry(1), entry(2)], [entry(3)])
    expect(merged.map((e) => e.line)).toEqual([1, 2, 3])
  })

  test('dedupes a socket replay that overlaps the lines already shown', () => {
    const existing = [entry(1), entry(2), entry(3)]
    const merged = mergeLogEntries(existing, [entry(2), entry(3), entry(4)])
    expect(merged.map((e) => e.line)).toEqual([1, 2, 3, 4])
  })

  test('returns the same array when nothing new arrived', () => {
    const existing = [entry(1), entry(2)]
    expect(mergeLogEntries(existing, [entry(1)])).toBe(existing)
    expect(mergeLogEntries(existing, [])).toBe(existing)
  })

  test('interleaves a full HTTP snapshot with a partial socket replay', () => {
    const socket = [entry(5), entry(6)]
    const snapshot = [entry(1), entry(2), entry(3), entry(4), entry(5)]
    expect(mergeLogEntries(snapshot, socket).map((e) => e.line)).toEqual([
      1, 2, 3, 4, 5, 6,
    ])
    expect(mergeLogEntries(socket, snapshot).map((e) => e.line)).toEqual([
      1, 2, 3, 4, 5, 6,
    ])
  })
})

describe('deriveJobLogView', () => {
  test('a finished job shows a skeleton while its log loads', () => {
    expect(view({})).toEqual({ body: 'loading', notice: 'none' })
  })

  test('a finished job with lines shows them', () => {
    expect(
      view({
        entryCount: 3,
        snapshot: {
          data: { kind: 'content', entries: [] },
          isPending: false,
          isError: false,
        },
      }).body
    ).toBe('lines')
  })

  test('a finished job whose log is gone (404) says so', () => {
    expect(
      view({
        snapshot: {
          data: { kind: 'gone', detail: 'no longer available' },
          isPending: false,
          isError: false,
        },
      })
    ).toEqual({ body: 'gone', notice: 'none' })
  })

  test('a finished job with an empty log reports no output', () => {
    expect(
      view({
        snapshot: {
          data: { kind: 'content', entries: [] },
          isPending: false,
          isError: false,
        },
      }).body
    ).toBe('no-output')
  })

  test('a failed read is an error, never an endless loading state', () => {
    expect(
      view({
        snapshot: { data: undefined, isPending: false, isError: true },
      })
    ).toEqual({ body: 'error', notice: 'none' })
  })

  test('a job that has not started (empty 200) says it has not started', () => {
    expect(
      view({
        phase: 'waiting',
        snapshot: {
          data: { kind: 'content', entries: [] },
          isPending: false,
          isError: false,
        },
      }).body
    ).toBe('not-started')
    // Also while the very first poll is still in flight.
    expect(view({ phase: 'waiting' }).body).toBe('not-started')
  })

  test('a running job connects, then waits for output on an open socket', () => {
    expect(view({ phase: 'live', socketState: 'idle' })).toEqual({
      body: 'connecting',
      notice: 'none',
    })
    expect(view({ phase: 'live', socketState: 'connecting' }).body).toBe(
      'connecting'
    )
    expect(view({ phase: 'live', socketState: 'open' }).body).toBe(
      'waiting-for-output'
    )
  })

  test('a running job whose socket failed polls with a visible notice', () => {
    expect(
      view({
        phase: 'live',
        socketState: 'failed',
        snapshot: {
          data: { kind: 'content', entries: [] },
          isPending: false,
          isError: false,
        },
      })
    ).toEqual({ body: 'polling-for-output', notice: 'polling' })
    expect(
      view({ phase: 'live', socketState: 'failed', entryCount: 12 })
    ).toEqual({ body: 'lines', notice: 'polling' })
  })

  test('a failing poll keeps the lines and flags the refresh failure', () => {
    expect(
      view({
        phase: 'live',
        socketState: 'failed',
        entryCount: 12,
        snapshot: { data: undefined, isPending: false, isError: true },
      })
    ).toEqual({ body: 'lines', notice: 'refresh-failed' })
  })

  test('a resumed socket clears the polling notice', () => {
    expect(view({ phase: 'live', socketState: 'open', entryCount: 4 })).toEqual(
      { body: 'lines', notice: 'none' }
    )
  })
})

describe('bounded viewer buffer', () => {
  test('parses only the most recent lines of a long log', () => {
    const content = jsonl(...[1, 2, 3, 4, 5].map((line) => entry(line)))
    expect(parseJobLogContent(content, now, 2).map((e) => e.line)).toEqual([
      4, 5,
    ])
  })

  test('drops the oldest lines once the cap is reached', () => {
    const merged = mergeLogEntries([entry(1), entry(2)], [entry(3)], 2)
    expect(merged.map((e) => e.line)).toEqual([2, 3])
    const keyed = mergeLogEntries([entry(2), entry(3)], [entry(1), entry(4)], 2)
    expect(keyed.map((e) => e.line)).toEqual([3, 4])
  })

  test('caps the default buffer at MAX_VIEWER_LINES', () => {
    const many = Array.from({ length: MAX_VIEWER_LINES + 5 }, (_, i) =>
      entry(i + 1)
    )
    const merged = mergeLogEntries([], many)
    expect(merged).toHaveLength(MAX_VIEWER_LINES)
    expect(merged[0].line).toBe(6)
  })
})

describe('partial logs', () => {
  test('streamed lines of a job whose full log is gone are flagged as partial', () => {
    expect(
      deriveJobLogView({
        phase: 'finished',
        socketState: 'idle',
        entryCount: 40,
        snapshot: {
          data: { kind: 'gone', detail: 'no longer available' },
          isPending: false,
          isError: false,
        },
      })
    ).toEqual({ body: 'lines', notice: 'partial-log' })
  })
})

test('live polling requests a small tail and completion reads the viewer window', () => {
  expect(snapshotTailLines('waiting')).toBe(128)
  expect(snapshotTailLines('live')).toBe(128)
  expect(snapshotTailLines('finished')).toBe(MAX_VIEWER_LINES)
})

test('successive polls retain overlapping history and report missed bursts', () => {
  const first = [entry(1), entry(2), entry(3)]
  const second = mergeLogEntries(first, [entry(3), entry(4), entry(5)])
  expect(second.map((item) => item.line)).toEqual([1, 2, 3, 4, 5])
  expect(missingLogLines(second)).toBe(0)
  const burst = mergeLogEntries(second, [entry(10), entry(11)])
  expect(missingLogLines(burst)).toBe(4)
  expect(
    missingLogLines(
      mergeLogEntries(burst, [entry(6), entry(7), entry(8), entry(9)])
    )
  ).toBe(0)
  expect(missingLogLines([entry(100), entry(101)])).toBe(0)
})
