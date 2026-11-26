// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import type { RestoreRunView } from '@/api/client/types.gen'
import {
  awaitingRestoredInventory,
  candidateDatabases,
  databaseRole,
  defaultChoice,
  expectedLine,
  missingProjects,
  needsConfirmation,
  restoredSummary,
  RESTORED_INVENTORY_WAIT_MS,
} from './restored-databases'
import { missing, report } from './restored-databases.fixture'

describe('restored databases', () => {
  it('groups the missing databases by project', () => {
    const [shop, ...rest] = missingProjects(report.missing)
    expect(rest).toHaveLength(0)
    expect(expectedLine(shop)).toBe(
      '(production, staging) reads shop_production and shop_staging'
    )
    expect(shop.suggested).toBe('shop_prod')
    expect(needsConfirmation(shop)).toBe(true)
  })

  it('offers only unused databases a link can be pointed at', () => {
    expect(candidateDatabases(report)).toEqual(['shop_prod'])
  })

  it('preselects the suggestion, else the first candidate', () => {
    const [shop] = missingProjects(report.missing)
    expect(defaultChoice(shop, ['other', 'shop_prod'])).toBe('shop_prod')
    expect(defaultChoice({ ...shop, suggested: null }, ['other'])).toBe('other')
    expect(defaultChoice(shop, [])).toBeNull()
  })

  it('does not ask to confirm a project with one environment', () => {
    const [single] = missingProjects([
      missing({ environment_names: ['production'] }),
    ])
    expect(needsConfirmation(single)).toBe(false)
    const [wide] = missingProjects([
      missing({ environment_name: null, expected_database: 'shop' }),
    ])
    expect(expectedLine(wide)).toBe('reads shop')
  })

  it('says who reads each database', () => {
    expect(databaseRole(report.databases[1])).toBe(
      'Read by Blog, another project'
    )
    expect(databaseRole(report.databases[3])).toBe('No linked project reads it')
    expect(databaseRole(report.databases[0])).toBe("This service's database")
  })

  it('summarises what came back, marking a cut list', () => {
    expect(restoredSummary(report)).toBe('4 databases and 1 user')
    expect(restoredSummary({ ...report, truncated: true })).toBe(
      '4 databases+ and 1 user+'
    )
  })
})

describe('waiting for what a restore recorded', () => {
  const finished = Date.UTC(2026, 10, 26, 12, 0, 0)
  const run = (overrides: Partial<RestoreRunView> = {}): RestoreRunView => ({
    id: 22,
    created_at: '2026-11-26T11:50:00Z',
    finished_at: new Date(finished).toISOString(),
    mode: 'in_place',
    phase: 'completed',
    source_backup: { id: 3 },
    cancellable: false,
    source_service_id: 5,
    status: 'completed',
    ...overrides,
  })

  it('asks again while the report predates a restore that just finished', () => {
    expect(awaitingRestoredInventory([run()], report, finished + 1000)).toBe(
      true
    )
    expect(awaitingRestoredInventory([run()], null, finished + 1000)).toBe(true)
  })

  it('stops once the report describes that restore', () => {
    expect(
      awaitingRestoredInventory(
        [run()],
        { ...report, restore_run_id: 22 },
        finished + 1000
      )
    ).toBe(false)
  })

  it('stops after the wait, when nothing was recorded', () => {
    expect(
      awaitingRestoredInventory(
        [run()],
        null,
        finished + RESTORED_INVENTORY_WAIT_MS + 1
      )
    ).toBe(false)
  })

  it('ignores running, failed and new-service runs', () => {
    const now = finished + 1000
    expect(
      awaitingRestoredInventory(
        [
          run({ status: 'running', finished_at: null }),
          run({ id: 23, status: 'failed' }),
          run({ id: 24, target_service_id: 9 }),
          run({ id: 25, target_service_name: 'orders-copy' }),
        ],
        report,
        now
      )
    ).toBe(false)
    expect(awaitingRestoredInventory(undefined, report, now)).toBe(false)
  })
})
