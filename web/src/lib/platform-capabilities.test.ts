// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  persistentWorkspaceStorageSupported,
  sourceArchiveUploadsSupported,
} from './platform-capabilities'

describe('platform capability presentation', () => {
  test('disables archive uploads for stateless control planes', () => {
    expect(sourceArchiveUploadsSupported({ stateless: true })).toBe(false)
    expect(sourceArchiveUploadsSupported({ stateless: false })).toBe(true)
  })

  test('keeps actions disabled until capabilities load', () => {
    expect(sourceArchiveUploadsSupported(undefined)).toBe(false)
    expect(
      persistentWorkspaceStorageSupported({ persistent_workspaces: false })
    ).toBe(false)
    expect(persistentWorkspaceStorageSupported(undefined)).toBe(false)
    expect(
      persistentWorkspaceStorageSupported({ persistent_workspaces: true })
    ).toBe(true)
  })
})
