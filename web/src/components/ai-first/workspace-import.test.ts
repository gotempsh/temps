// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  batchLocalImportFiles,
  prepareLocalImport,
  shouldSkipLocalImportPath,
} from './workspace-import'

function folderFile(path: string, contents = 'x'): File {
  const parts = path.split('/')
  const file = new File([contents], parts[parts.length - 1] ?? path)
  Object.defineProperty(file, 'webkitRelativePath', { value: path })
  return file
}

describe('workspace import', () => {
  test('strips the selected root and excludes dependencies and secrets', () => {
    const selection = prepareLocalImport([
      folderFile('site/src/index.ts'),
      folderFile('site/.env', 'SECRET=yes'),
      folderFile('site/node_modules/pkg/index.js'),
      folderFile('site/cert.pem'),
    ])

    expect(selection.rootName).toBe('site')
    expect(selection.accepted.map((file) => file.path)).toEqual([
      'src/index.ts',
    ])
    expect(selection.skipped).toHaveLength(3)
  })

  test('recognizes credential-like paths consistently', () => {
    expect(shouldSkipLocalImportPath('packages/web/.env.local')).toBe(true)
    expect(shouldSkipLocalImportPath('keys/id_ed25519')).toBe(true)
    expect(shouldSkipLocalImportPath('.Docker/config.json')).toBe(true)
    expect(shouldSkipLocalImportPath('infra/terraform.tfstate')).toBe(true)
    expect(shouldSkipLocalImportPath('auth/service-account-prod.json')).toBe(
      true
    )
    expect(shouldSkipLocalImportPath('src/environment.ts')).toBe(false)
  })

  test('batches uploads without exceeding the per-request file cap', () => {
    const files = Array.from({ length: 33 }, (_, index) => ({
      file: folderFile(`site/file-${index}.txt`),
      path: `file-${index}.txt`,
    }))
    expect(batchLocalImportFiles(files).map((batch) => batch.length)).toEqual([
      32, 1,
    ])
  })
})
