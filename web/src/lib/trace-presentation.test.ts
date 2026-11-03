// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  durationBarClass,
  durationShare,
  formatTraceDuration,
  traceOutcome,
} from './trace-presentation'
test('formats trace durations without noisy millisecond decimals', () => {
  expect(formatTraceDuration(17839.72)).toBe('17.8 s')
  expect(formatTraceDuration(3.6)).toBe('4 ms')
  expect(formatTraceDuration(0.04)).toBe('40 µs')
  expect(formatTraceDuration(0)).toBe('0 ms')
  expect(formatTraceDuration(0.0001)).toBe('<1 µs')
  expect(formatTraceDuration(NaN)).toBe('—')
  expect(formatTraceDuration(-1)).toBe('—')
})

test('a trace is an error when a span errored or the root says so, and unset is not a failure', () => {
  expect(traceOutcome({ error_count: 2, status_code: 'OK' })).toBe('error')
  expect(traceOutcome({ error_count: 0, status_code: 'ERROR' })).toBe('error')
  expect(traceOutcome({ error_count: 0, status_code: 'OK' })).toBe('ok')
  expect(traceOutcome({ error_count: 0, status_code: 'UNSET' })).toBe('unset')
  expect(traceOutcome({ error_count: 0 })).toBe('unset')
})

test('duration bars scale to the slowest trace and never vanish for a real duration', () => {
  expect(durationShare(500, 1000)).toBe(0.5)
  expect(durationShare(1000, 1000)).toBe(1)
  expect(durationShare(0.01, 1000)).toBe(0.04)
  expect(durationShare(0, 1000)).toBe(0)
  expect(durationShare(5, 0)).toBe(0)
  expect(durationShare(NaN, 1000)).toBe(0)
})

test('duration bars are coloured by latency band', () => {
  expect(durationBarClass(12)).toBe('bg-emerald-500')
  expect(durationBarClass(250)).toBe('bg-amber-500')
  expect(durationBarClass(1500)).toBe('bg-orange-500')
  expect(durationBarClass(9000)).toBe('bg-red-500')
})
