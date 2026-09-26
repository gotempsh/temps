// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  collectProjectId,
  formatEta,
  formatPercent,
  isTerminal,
  parseTimestamp,
} from './bulk-activation.js'

describe('formatEta', () => {
  test('says "estimating…" before the server has observed any throughput', () => {
    // The server sends `eta_state: "estimating"` with no number precisely so
    // the client does not invent one. Showing "0s" here would tell a customer
    // watching a multi-hour activation that it was about to finish.
    expect(formatEta(undefined, 'estimating')).toBe('estimating…')
    expect(formatEta(1234, 'estimating')).toBe('estimating…')
  })

  test('shows nothing for a job that has already stopped', () => {
    expect(formatEta(undefined, 'finished')).toBe('—')
    expect(formatEta(500, 'finished')).toBe('—')
  })

  test('renders coarsely rather than as a jumping countdown', () => {
    // The ETA is an average over acknowledged chunks, not a schedule. False
    // precision that jumps around is worse than an honest range.
    expect(formatEta(30, 'known')).toBe('under a minute')
    expect(formatEta(600, 'known')).toBe('about 10 minute(s)')
    expect(formatEta(9000, 'known')).toBe('about 2.5 hour(s)')
    expect(formatEta(180_000, 'known')).toBe('about 2.1 day(s)')
  })

  test('a known state with no number still degrades to "estimating…"', () => {
    expect(formatEta(undefined, 'known')).toBe('estimating…')
  })
})

describe('formatPercent', () => {
  test('an absent percentage is not zero', () => {
    // The server omits `percent_complete` when the estimate is zero. "0%"
    // would read as stuck; the window simply has nothing in it.
    expect(formatPercent(undefined)).toBe('—')
  })

  test('renders one decimal place', () => {
    expect(formatPercent(0)).toBe('0.0%')
    expect(formatPercent(42.567)).toBe('42.6%')
    expect(formatPercent(100)).toBe('100.0%')
  })
})

describe('isTerminal', () => {
  test('only pending and running keep the job open', () => {
    // `--watch` polls until this returns true. Getting it wrong either loops
    // forever on a finished job or stops watching one that is still spending.
    expect(isTerminal('pending')).toBe(false)
    expect(isTerminal('running')).toBe(false)
    for (const status of [
      'completed',
      'completed_with_failures',
      'aborted',
      'cancelled',
    ] as const) {
      expect(isTerminal(status)).toBe(true)
    }
  })
})

describe('collectProjectId', () => {
  test('accumulates repeated --project flags', () => {
    expect(collectProjectId('9', collectProjectId('4', []))).toEqual([4, 9])
  })

  test('rejects a non-numeric id rather than sending NaN to the server', () => {
    expect(() => collectProjectId('my-app', [])).toThrow('Invalid --project')
  })

  test('rejects zero and negative ids', () => {
    expect(() => collectProjectId('0', [])).toThrow('Invalid --project')
    expect(() => collectProjectId('-3', [])).toThrow('Invalid --project')
  })
})

describe('parseTimestamp', () => {
  test('an omitted window is left to the server default', () => {
    expect(parseTimestamp(undefined, '--from')).toBeUndefined()
  })

  test('normalises an RFC 3339 timestamp to UTC', () => {
    expect(parseTimestamp('2026-08-01T00:00:00Z', '--from')).toBe('2026-08-01T00:00:00.000Z')
    expect(parseTimestamp('2026-08-01T02:00:00+02:00', '--from')).toBe(
      '2026-08-01T00:00:00.000Z',
    )
  })

  test('names the flag and shows the expected form when it cannot parse', () => {
    // A window that silently became the epoch would quote — and then ship —
    // a completely different range than the operator asked for.
    expect(() => parseTimestamp('last tuesday', '--from')).toThrow('--from')
    expect(() => parseTimestamp('last tuesday', '--from')).toThrow('RFC 3339')
  })
})
