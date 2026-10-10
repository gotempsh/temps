// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  isRepositoryRootDirectory,
  rootDirectoryHelp,
} from './project-directory'

describe('rootDirectoryHelp', () => {
  test('shows the server refusal verbatim', () => {
    const refusal =
      "Root directory 'apps/apii' does not exist in example/monorepo on branch 'main': 'apps' has no 'apii'; it contains: api, web."
    expect(rootDirectoryHelp(refusal, false, 'main')).toBe(refusal)
  })

  test('says a Git directory is checked on the configured branch', () => {
    expect(rootDirectoryHelp(null, false, 'main')).toBe(
      'Must exist in the repository on branch main; it is checked when you save.'
    )
    expect(rootDirectoryHelp(null, false, undefined)).toBe(
      'Must exist in the repository; it is checked when you save.'
    )
  })

  test('does not promise a repository check for uploaded sources', () => {
    expect(rootDirectoryHelp(null, true, 'main')).toBe(
      'Relative to the root of the uploaded source.'
    )
  })
})

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
