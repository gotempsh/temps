// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { formatLogMessage, parseLogEntries } from './logs.js'

describe('parseLogEntries', () => {
  test('passes an array through untouched', () => {
    const entries = [{ message: 'hi' }]
    expect(parseLogEntries(entries)).toBe(entries)
  })

  test('returns nothing for non-string, non-array data', () => {
    expect(parseLogEntries(undefined)).toEqual([])
    expect(parseLogEntries(42)).toEqual([])
  })

  test('parses each JSONL line into a structured entry', () => {
    const raw = [
      JSON.stringify({ timestamp: '2026-01-01T00:00:00Z', level: 'info', message: 'starting', line: 3 }),
      JSON.stringify({ message: 'no line number given' }),
    ].join('\n')

    expect(parseLogEntries(raw)).toEqual([
      { timestamp: '2026-01-01T00:00:00Z', level: 'info', message: 'starting', line: 3 },
      { timestamp: undefined, level: undefined, message: 'no line number given', line: 2 },
    ])
  })

  test('falls back to a plain-text message when a line is not valid JSON', () => {
    // Build logs are not always JSON — a raw stdout line must still show up,
    // not silently vanish from the log stream.
    const raw = 'Building image...\nStep 1/5 : FROM node:20'

    expect(parseLogEntries(raw)).toEqual([
      { message: 'Building image...', line: 1 },
      { message: 'Step 1/5 : FROM node:20', line: 2 },
    ])
  })

  test('drops blank lines instead of emitting empty entries', () => {
    const raw = 'first\n\n\nsecond\n'
    expect(parseLogEntries(raw)).toEqual([
      { message: 'first', line: 1 },
      { message: 'second', line: 2 },
    ])
  })
})

describe('formatLogMessage', () => {
  test('renders a message with no level as plain text', () => {
    expect(formatLogMessage({ message: 'plain message' })).toBe('plain message')
  })

  test('includes the message for every known level', () => {
    for (const level of ['info', 'success', 'warning', 'error']) {
      expect(formatLogMessage({ message: 'hello', level })).toContain('hello')
    }
  })

  test('falls back to a muted color for an unrecognized level rather than throwing', () => {
    expect(formatLogMessage({ message: 'weird', level: 'trace' })).toContain('weird')
  })

  test('prefixes a formatted timestamp when present', () => {
    const out = formatLogMessage({ message: 'msg', timestamp: '2026-01-01T00:00:00Z' })
    expect(out).toContain('msg')
    expect(out.length).toBeGreaterThan('msg'.length)
  })
})
