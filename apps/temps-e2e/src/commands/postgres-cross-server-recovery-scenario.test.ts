// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { startupTail } from './postgres-cross-server-recovery-scenario.ts'

test('startup diagnostics retain the latest error and redact credentials', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'recovery-log-'))
  try {
    const path = join(dir, 'startup.log')
    await Bun.write(
      path,
      'old line\n'.repeat(1500) + 'password=private\nsetup value temporary-value\nport already in use\n',
    )
    const tail = await startupTail(path, 'temporary-value')
    expect(tail.split('\n').length).toBeLessThanOrEqual(30)
    expect(tail).toContain('port already in use')
    expect(tail).not.toContain('private')
    expect(tail).not.toContain('temporary-value')
  } finally {
    await rm(dir, { recursive: true, force: true })
  }
})
