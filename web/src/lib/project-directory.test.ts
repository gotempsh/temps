// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { isRepositoryRootDirectory } from './project-directory'

describe('isRepositoryRootDirectory', () => {
  test.each([null, undefined, '', '.', './', '/', ' / ', '///', '\\'])(
    'recognizes %p as the repository root',
    (directory) => {
      expect(isRepositoryRootDirectory(directory)).toBe(true)
    }
  )

  test.each(['apps/web', './apps/web', '/apps/web', ' .hidden '])(
    'recognizes %p as a subdirectory',
    (directory) => {
      expect(isRepositoryRootDirectory(directory)).toBe(false)
    }
  )
})
