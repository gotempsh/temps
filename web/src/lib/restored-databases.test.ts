// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  candidateDatabases,
  databaseRole,
  defaultChoice,
  expectedLine,
  missingProjects,
  needsConfirmation,
  restoredSummary,
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
