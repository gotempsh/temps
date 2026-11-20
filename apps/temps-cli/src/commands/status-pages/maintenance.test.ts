// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { parseDuration, parseWhen, scheduleBody, stateLabel, updateBody } from './maintenance.js'

const NOW = new Date('2026-11-20T10:00:00Z')

describe('parseDuration', () => {
  it('reads minutes, hours and days', () => {
    expect(parseDuration('90m')).toBe(90 * 60_000)
    expect(parseDuration('2h')).toBe(2 * 3_600_000)
    expect(parseDuration('1d')).toBe(86_400_000)
    expect(parseDuration('0h')).toBeNull()
    expect(parseDuration('2 weeks')).toBeNull()
  })
})

describe('parseWhen', () => {
  it('needs a zone so the time is not guessed', () => {
    expect(parseWhen('now', NOW)).toBe(NOW)
    expect(parseWhen('2026-12-01T22:00:00Z', NOW)?.toISOString()).toBe('2026-12-01T22:00:00.000Z')
    expect(parseWhen('2026-12-01T22:00:00+01:00', NOW)?.toISOString()).toBe('2026-12-01T21:00:00.000Z')
    expect(parseWhen('2026-12-01T22:00:00', NOW)).toBeNull()
    expect(parseWhen('tomorrow', NOW)).toBeNull()
  })
})

describe('scheduleBody', () => {
  const base = { title: 'Database upgrade', starts: '2026-12-01T22:00:00Z', monitor: ['4,5', '5'] }

  it('builds the request from --duration', () => {
    expect(scheduleBody({ ...base, duration: '1h' }, NOW)).toEqual({
      title: 'Database upgrade',
      description: null,
      starts_at: '2026-12-01T22:00:00.000Z',
      ends_at: '2026-12-01T23:00:00.000Z',
      monitor_ids: [4, 5],
    })
  })

  it('says what is missing or wrong', () => {
    const problem = (options: Parameters<typeof scheduleBody>[0]) => scheduleBody(options, NOW) as string
    expect(problem({ ...base, title: ' ', duration: '1h' })).toContain('--title')
    expect(problem({ ...base })).toContain('--ends or --duration')
    expect(problem({ ...base, ends: '2026-12-01T23:00:00Z', duration: '1h' })).toContain('not both')
    expect(problem({ ...base, ends: '2026-12-01T21:00:00Z' })).toContain('end after')
    expect(problem({ ...base, duration: '8d' })).toContain('7 days')
    expect(problem({ ...base, duration: '1h', monitor: [] })).toContain('--monitor')
    expect(problem({ ...base, duration: '1h', monitor: ['x'] })).toContain('Invalid monitor ID')
  })
})

describe('updateBody', () => {
  it('sends only what was passed', () => {
    expect(updateBody({ ends: '2026-12-01T23:30:00Z' }, NOW)).toEqual({ ends_at: '2026-12-01T23:30:00.000Z' })
    expect(updateBody({ description: '' }, NOW)).toEqual({ description: '' })
    expect(typeof updateBody({}, NOW)).toBe('string')
    expect(typeof updateBody({ duration: '1h' }, NOW)).toBe('string')
  })

  it('labels states for people', () => {
    expect(stateLabel('in_progress')).toBe('in progress')
    expect(stateLabel('scheduled')).toBe('scheduled')
  })
})
