// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { repositoryFilePath } from './repository-file-path'

describe('repositoryFilePath', () => {
  test('keeps a Compose file at the repository root unchanged', () => {
    expect(repositoryFilePath('./', 'compose.yaml')).toBe('compose.yaml')
  })

  test('resolves a custom Compose file relative to the project root', () => {
    expect(
      repositoryFilePath(
        'nextcloud-postgres',
        'infrastructure/compose.production.yaml'
      )
    ).toBe('nextcloud-postgres/infrastructure/compose.production.yaml')
  })

  test('normalizes path separators and leading dot segments', () => {
    expect(repositoryFilePath('./apps/web/', './deploy\\compose.yml')).toBe(
      'apps/web/deploy/compose.yml'
    )
  })

  test('does not prepend the root twice for legacy full paths', () => {
    expect(
      repositoryFilePath(
        'nextcloud-postgres',
        'nextcloud-postgres/compose.yaml'
      )
    ).toBe('nextcloud-postgres/compose.yaml')
  })
})
