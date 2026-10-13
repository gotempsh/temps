// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { describeConsoleAccess } from './console-access.js'
import type { CloudStatus } from '../../api/types.gen.js'

function statusFixture(overrides: Partial<CloudStatus> = {}): CloudStatus {
  return {
    account_email: null,
    backend_url: 'https://cloud.example.test',
    backups_enabled: false,
    console_access_enabled: false,
    health: 'healthy',
    health_message: '',
    instance_id: null,
    managed_backup_setup: {
      action: 'none',
      archive_conflicts: [],
      message: '',
      ready: false,
      status: 'disabled',
    },
    notifications_enabled: false,
    spooled_spans: 0,
    status: 'not_linked',
    status_message: '',
    telemetry_enabled: false,
    ...overrides,
  }
}

describe('describeConsoleAccess', () => {
  test('tells an unlinked instance to connect first', () => {
    const message = describeConsoleAccess(statusFixture({ status: 'not_linked' }))
    expect(message).toContain('not connected to Temps Cloud')
    expect(message).toContain('temps cloud connect')
  })

  test('tells a linked-but-off instance how to enable it', () => {
    const message = describeConsoleAccess(
      statusFixture({ status: 'linked', console_access_enabled: false })
    )
    expect(message).toContain('Off')
    expect(message).toContain('console-access enable')
  })

  test('confirms the console is reachable once linked and on', () => {
    const message = describeConsoleAccess(
      statusFixture({ status: 'linked', console_access_enabled: true })
    )
    expect(message).toContain('On')
    expect(message).toContain('owner or admin role')
  })
})
