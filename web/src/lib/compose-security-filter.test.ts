// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { filterComposeSecurityChecks } from './compose-security-filter'

const checks = [
  {
    id: 'service_shm',
    group: 'Resources',
    label: 'Limit service shared memory',
    consequence: 'Configure more than 512 MiB per service.',
  },
  {
    id: 'no_new_privileges',
    group: 'Runtime',
    label: 'Prevent sudo and setuid privilege elevation',
    consequence: 'Allow sudo to gain root privileges.',
  },
  {
    id: 'aggregate_shm',
    group: 'Resources',
    label: 'Limit aggregate shared memory',
    consequence: 'Configure more than 1 GiB per stack.',
  },
]

describe('filterComposeSecurityChecks', () => {
  test('shows all, enabled, and disabled checks from the current policy', () => {
    const disabled = ['service_shm']

    expect(filterComposeSecurityChecks(checks, disabled, '', 'all')).toEqual(
      checks
    )
    expect(
      filterComposeSecurityChecks(checks, disabled, '', 'enabled')
    ).toEqual(checks.slice(1))
    expect(
      filterComposeSecurityChecks(checks, disabled, '', 'disabled')
    ).toEqual(checks.slice(0, 1))

    expect(
      filterComposeSecurityChecks(
        checks,
        ['service_shm', 'no_new_privileges'],
        '',
        'disabled'
      )
    ).toEqual(checks.slice(0, 2))
  })

  test('combines status with case-insensitive search across labels and groups', () => {
    expect(
      filterComposeSecurityChecks(
        checks,
        ['service_shm'],
        '  RESOURCES ',
        'enabled'
      )
    ).toEqual(checks.slice(2))
    expect(
      filterComposeSecurityChecks(checks, ['service_shm'], 'sudo', 'disabled')
    ).toEqual([])
    expect(
      filterComposeSecurityChecks(checks, ['service_shm'], 'sudo', 'all')
    ).toEqual(checks.slice(1, 2))
  })
})
