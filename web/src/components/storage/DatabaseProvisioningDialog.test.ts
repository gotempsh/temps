// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  buildDatabaseProvisioningSelection,
  projectServiceResourcePath,
} from '@/lib/database-provisioning'

describe('database provisioning selection', () => {
  test('keeps non-custom modes free of a stale custom name', () => {
    expect(
      buildDatabaseProvisioningSelection('project_environment', 'stale_name')
    ).toEqual({ database_provisioning_mode: 'project_environment' })
    expect(buildDatabaseProvisioningSelection('project', 'stale_name')).toEqual(
      { database_provisioning_mode: 'project' }
    )
  })

  test('preserves an exact safe custom database name', () => {
    expect(
      buildDatabaseProvisioningSelection('custom', 'shared_catalog_2')
    ).toEqual({
      database_provisioning_mode: 'custom',
      custom_database_name: 'shared_catalog_2',
    })
  })

  test('rejects custom names providers cannot safely provision', () => {
    for (const name of ['', 'SharedCatalog', '2catalog', 'shared-catalog']) {
      expect(buildDatabaseProvisioningSelection('custom', name)).toBeNull()
    }
  })
})

describe('project database resource paths', () => {
  test('matches relational provider normalization and the 63-byte limit', () => {
    const slug = `project-${'a'.repeat(80)}`
    const expected = `project_${'a'.repeat(55)}`

    for (const serviceType of ['postgres', 'mariadb']) {
      expect(
        projectServiceResourcePath(serviceType, slug, 'production', {
          database_provisioning_mode: 'project',
        })
      ).toBe(expected)
      expect(expected).toHaveLength(63)
    }
  })

  test('matches all relational isolation modes', () => {
    expect(projectServiceResourcePath('postgres', 'my-project')).toBe(
      'my_project_production'
    )
    expect(
      projectServiceResourcePath('postgres', 'my-project', 'production', {
        database_provisioning_mode: 'project',
      })
    ).toBe('my_project')
    expect(
      projectServiceResourcePath('postgres', 'my-project', 'production', {
        database_provisioning_mode: 'custom',
        custom_database_name: 'shared_catalog',
      })
    ).toBe('shared_catalog')
  })

  test('preserves MongoDB scoped names verbatim', () => {
    expect(projectServiceResourcePath('mongodb', 'my-project')).toBe(
      'my-project_production'
    )
    expect(
      projectServiceResourcePath('mongodb', 'my-project', 'production', {
        database_provisioning_mode: 'project',
      })
    ).toBe('my-project')
  })
})
