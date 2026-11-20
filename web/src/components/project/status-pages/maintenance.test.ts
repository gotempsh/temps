// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import type { MaintenanceResponse } from '@/api/client'
import {
  createMaintenanceRequest,
  fromLocalInput,
  isOpen,
  maintenanceSchema,
  newMaintenanceValues,
  toLocalInput,
  updateMaintenanceRequest,
  type MaintenanceValues,
} from './maintenance'

const NOW = new Date(2026, 10, 20, 10, 17)

function values(overrides: Partial<MaintenanceValues> = {}): MaintenanceValues {
  return {
    title: 'Database upgrade',
    description: '',
    startsAt: toLocalInput(new Date(2026, 10, 20, 22, 0)),
    endsAt: toLocalInput(new Date(2026, 10, 20, 23, 0)),
    monitorIds: [4],
    ...overrides,
  }
}

function errors(
  input: MaintenanceValues,
  started = false
): Record<string, string> {
  const result = maintenanceSchema(() => NOW, started).safeParse(input)
  if (result.success) return {}
  return Object.fromEntries(
    result.error.issues.map((issue) => [issue.path.join('.'), issue.message])
  )
}

describe('local datetime inputs', () => {
  it('round-trips in the browser zone', () => {
    const value = toLocalInput(NOW)
    expect(value).toBe('2026-11-20T10:17')
    expect(fromLocalInput(value)?.getTime()).toBe(NOW.getTime())
    expect(fromLocalInput('')).toBeNull()
  })
})

describe('maintenanceSchema', () => {
  it('accepts a future window', () => {
    expect(errors(values())).toEqual({})
  })

  it('needs a title and at least one component', () => {
    const found = errors(values({ title: ' ', monitorIds: [] }))
    expect(found.title).toContain('title')
    expect(found.monitorIds).toContain('component')
  })

  it('refuses a backdated start, but not on a window already running', () => {
    const backdated = values({
      startsAt: toLocalInput(new Date(2026, 10, 20, 9, 0)),
      endsAt: toLocalInput(new Date(2026, 10, 20, 12, 0)),
    })
    expect(errors(backdated).startsAt).toContain('past')
    expect(errors(backdated, true)).toEqual({})
  })

  it('bounds the end', () => {
    expect(
      errors(values({ endsAt: toLocalInput(new Date(2026, 10, 20, 21, 0)) }))
        .endsAt
    ).toContain('after')
    expect(
      errors(values({ endsAt: toLocalInput(new Date(2026, 10, 28, 23, 0)) }))
        .endsAt
    ).toContain('7 days')
    expect(
      errors(
        values({
          startsAt: toLocalInput(new Date(2026, 10, 20, 8, 0)),
          endsAt: toLocalInput(new Date(2026, 10, 20, 9, 0)),
        }),
        true
      ).endsAt
    ).toContain('future')
  })
})

describe('requests', () => {
  it('starts a new window at the next hour for an hour', () => {
    const fresh = newMaintenanceValues(NOW, [1, 2])
    expect(fresh.startsAt).toBe('2026-11-20T11:00')
    expect(fresh.endsAt).toBe('2026-11-20T12:00')
    expect(fresh.monitorIds).toEqual([1, 2])
  })

  it('sends UTC instants and drops an empty description', () => {
    const body = createMaintenanceRequest(values())
    expect(body.starts_at).toBe(new Date(2026, 10, 20, 22, 0).toISOString())
    expect(body.description).toBeNull()
    expect(body.monitor_ids).toEqual([4])
  })

  it('only sends what a running window may change', () => {
    const running = updateMaintenanceRequest(values({ description: 'x' }), true)
    expect(running).not.toHaveProperty('starts_at')
    expect(running).not.toHaveProperty('monitor_ids')
    expect(running.description).toBe('x')
    const scheduled = updateMaintenanceRequest(values(), false)
    expect(scheduled.monitor_ids).toEqual([4])
    // An empty description removes the stored one.
    expect(scheduled.description).toBe('')
  })

  it('treats scheduled and running windows as open', () => {
    const item = (state: MaintenanceResponse['state']) =>
      ({ state }) as MaintenanceResponse
    expect(isOpen(item('scheduled'))).toBe(true)
    expect(isOpen(item('in_progress'))).toBe(true)
    expect(isOpen(item('completed'))).toBe(false)
    expect(isOpen(item('cancelled'))).toBe(false)
  })
})
