// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { linkDatabaseFields, registerServicesCommands, servicePlacement } from './index.js'
import { restoreLocation } from './restore.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('services create placement', () => {
  test('a cluster spreads its members over nodes', async () => {
    const run = await runWithApi(
      registerServicesCommands,
      ['services', 'create', '-t', 'postgres', '-n', 'db', '--topology', 'cluster', '--member', 'monitor', '--member', 'primary@3', '--member', 'replica@4', '--yes'],
      { 'GET /external-services/types': ['postgres'], 'POST /external-services': { id: 1 } },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toMatchObject({
      name: 'db',
      service_type: 'postgres',
      topology: 'cluster',
      members: [{ role: 'monitor' }, { role: 'primary', node_id: 3 }, { role: 'replica', node_id: 4 }],
    })
  })

  test('placement is checked before any request', () => {
    expect(servicePlacement({ node: '2' })).toEqual({ node_id: 2 })
    expect(() => servicePlacement({ topology: 'ha' })).toThrow(/--topology must be standalone or cluster/)
    expect(() => servicePlacement({ member: ['primary'] })).toThrow(/--member needs --topology cluster/)
    expect(() => servicePlacement({ topology: 'cluster', member: ['leader@1'] })).toThrow(/the role must be one of monitor, primary, replica/)
    expect(() => servicePlacement({ topology: 'cluster', member: ['primary@x'] })).toThrow(/--member primary@x/)
  })
})

describe('services link database selection', () => {
  test('a custom database needs its name, and only then', async () => {
    expect(linkDatabaseFields({})).toEqual({})
    expect(linkDatabaseFields({ databaseMode: 'project_environment' })).toEqual({ database_provisioning_mode: 'project_environment' })
    expect(linkDatabaseFields({ databaseMode: 'custom', databaseName: 'shop' })).toEqual({
      database_provisioning_mode: 'custom',
      custom_database_name: 'shop',
    })
    expect(() => linkDatabaseFields({ databaseMode: 'custom' })).toThrow(/custom needs --database-name/)
    expect(() => linkDatabaseFields({ databaseName: 'shop' })).toThrow(/--database-name goes with --database-mode custom/)
    expect(() => linkDatabaseFields({ databaseMode: 'shared' })).toThrow(/--database-mode must be one of/)
  })

  test('link sends the selection', async () => {
    const run = await runWithApi(
      registerServicesCommands,
      ['services', 'link', '--id', '4', '-p', 'web', '--database-mode', 'custom', '--database-name', 'shop'],
      { 'GET /projects/by-slug/web': { id: 7, slug: 'web', name: 'web' }, 'POST /external-services/4/projects': { ok: true } },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ project_id: 7, database_provisioning_mode: 'custom', custom_database_name: 'shop' })
  })
})

describe('services restore from an S3 location', () => {
  test('a location needs its engine and S3 source, and replaces --backup-id', () => {
    expect(restoreLocation({ backupId: '3' })).toBeUndefined()
    expect(restoreLocation({ backupLocation: 'backups/db/1.dump', backupEngine: 'postgres', s3Source: '2' })).toEqual({
      backup_location: 'backups/db/1.dump',
      backup_engine: 'postgres',
      s3_source_id: 2,
    })
    expect(() => restoreLocation({ backupLocation: 'k', s3Source: '2' })).toThrow(/needs --backup-engine/)
    expect(() => restoreLocation({ backupLocation: 'k', backupEngine: 'postgres' })).toThrow(/needs --s3-source/)
    expect(() => restoreLocation({ backupId: '3', backupLocation: 'k', backupEngine: 'postgres', s3Source: '2' })).toThrow(/not both/)
    expect(() => restoreLocation({ s3Source: '2' })).toThrow(/go with --backup-location/)
  })

  test('restore sends the location instead of a backup id', async () => {
    const run = await runWithApi(
      registerServicesCommands,
      ['services', 'restore', '--id', '4', '--backup-location', 'backups/db/1.dump', '--backup-engine', 'postgres', '--s3-source', '2', '--yes', '--no-wait', '--json'],
      {
        'GET /external-services/4/restore-capabilities': { restore_in_place: true, restore_to_new_service: true, pitr: false, suggested_new_service_name: 'db-restored' },
        'GET /external-services/4': { id: 4, name: 'db', service_type: 'postgres' },
        'POST /external-services/4/restore': { id: 9, status: 'pending', phase: 'queued' },
      },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({
      backup_location: 'backups/db/1.dump',
      backup_engine: 'postgres',
      s3_source_id: 2,
      confirm_cross_service: false,
      mode: 'in_place',
    })
  })
})
