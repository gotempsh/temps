// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { creationBackupHref } from './creation-backup-schedule'

test('setup requires an explicit destination', () => {
  for (const destination of ['later', '', '0', '-1', 'bad']) {
    expect(creationBackupHref(42, destination)).toBe('/storage/42')
  }
})
test('chosen destination opens schedule setup scoped to the new service', () => {
  const href = creationBackupHref(42, '7')
  expect(href).toContain('/backups/s3-sources/7/schedules/new?service_id=42')
  expect(href).toContain('returnTo=')
})
