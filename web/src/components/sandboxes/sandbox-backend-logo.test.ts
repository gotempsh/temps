// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { existsSync } from 'node:fs'
import { join } from 'node:path'
import { sandboxBackendLogo } from './sandbox-backend-logo'

describe('sandboxBackendLogo', () => {
  test('knows the three backends, whatever the casing', () => {
    expect(sandboxBackendLogo('docker')?.label).toBe('Docker')
    expect(sandboxBackendLogo('Firecracker')?.label).toBe('Firecracker')
    expect(sandboxBackendLogo('microsandbox')?.markOnly).toBe(true)
  })

  test('an unrecorded or unknown backend has no logo', () => {
    expect(sandboxBackendLogo(null)).toBeNull()
    expect(sandboxBackendLogo(undefined)).toBeNull()
    expect(sandboxBackendLogo('')).toBeNull()
    expect(sandboxBackendLogo('qemu')).toBeNull()
  })

  test('every logo points at a file that ships with the console', () => {
    for (const backend of ['docker', 'firecracker', 'microsandbox']) {
      const logo = sandboxBackendLogo(backend)
      expect(logo).not.toBeNull()
      expect(existsSync(join(process.cwd(), 'public', logo!.src))).toBe(true)
    }
  })
})
