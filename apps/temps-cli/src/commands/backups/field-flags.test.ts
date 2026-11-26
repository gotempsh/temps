// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerBackupsCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('backup flags for every request field', () => {
  test('schedules create targets services and bounds the run', async () => {
    const run = await runWithApi(
      registerBackupsCommands,
      ['backups', 'schedules', 'create', '-n', 'nightly', '-t', 'full', '-s', '0 0 3 * * *', '-r', '7', '--s3-source-id', '1', '--service-ids', '4', '--service-ids', '5', '--include-control-plane', '--max-runtime-secs', '3600', '-y'],
      { 'POST /backups/schedules': { id: 1, name: 'nightly' } },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies.at(-1)?.body).toMatchObject({ service_ids: [4, 5], include_control_plane: true, max_runtime_secs: 3600 })
  })

  test('sources list-backups can scan the bucket', async () => {
    const run = await runWithApi(registerBackupsCommands, ['backups', 'sources', 'backups', '--id', '2', '--include-s3-scan', '--json'], {
      'GET /backups/s3-sources/2/backups': { backups: [] },
    })
    expect(run.requests).toEqual(['GET /backups/s3-sources/2/backups?include_s3_scan=true'])
  })
})
