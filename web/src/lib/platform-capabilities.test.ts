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

  test('preserves legacy support when fields are omitted or discovery fails', () => {
    expect(sourceArchiveUploadsSupported(undefined)).toBe(true)
    expect(sourceArchiveUploadsSupported({})).toBe(true)
    expect(persistentWorkspaceStorageSupported(undefined)).toBe(true)
    expect(persistentWorkspaceStorageSupported({})).toBe(true)
    expect(persistentWorkspaceStorageSupported({ stateless: false })).toBe(true)
  })

  test('honors explicit workspace restrictions, including stateless mode', () => {
    expect(
      persistentWorkspaceStorageSupported({ persistent_workspaces: false })
    ).toBe(false)
    expect(persistentWorkspaceStorageSupported({ stateless: true })).toBe(false)
    expect(
      persistentWorkspaceStorageSupported({
        stateless: true,
        persistent_workspaces: true,
      })
    ).toBe(false)
    expect(
      persistentWorkspaceStorageSupported({ persistent_workspaces: true })
    ).toBe(true)
  })
})
