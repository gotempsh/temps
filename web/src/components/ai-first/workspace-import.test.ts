// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { strToU8, zipSync } from 'fflate'
import {
  batchLocalImportFiles,
  MAX_LOCAL_IMPORT_FILE_BYTES,
  prepareLocalImport,
  prepareWorkspaceImport,
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
    expect(shouldSkipLocalImportPath('.temps/runtime.json')).toBe(true)
    expect(shouldSkipLocalImportPath('.DS_Store')).toBe(true)
    expect(shouldSkipLocalImportPath('.yarnrc')).toBe(true)
    expect(shouldSkipLocalImportPath('auth/service-account-prod.json')).toBe(
      true
    )
    expect(shouldSkipLocalImportPath('.env.example')).toBe(false)
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

  test('starts a new request before the decoded byte cap is crossed', () => {
    const files = [
      {
        file: folderFile('first.bin', 'a'.repeat(3 * 1024 * 1024)),
        path: 'first.bin',
      },
      {
        file: folderFile('second.bin', 'b'.repeat(2 * 1024 * 1024)),
        path: 'second.bin',
      },
    ]

    expect(batchLocalImportFiles(files).map((batch) => batch.length)).toEqual([
      1, 1,
    ])
  })

  test('preserves multiple dropped roots instead of flattening their paths', () => {
    const selection = prepareLocalImport([
      { file: folderFile('alpha/src/a.ts'), path: 'alpha/src/a.ts' },
      { file: folderFile('beta/src/b.ts'), path: 'beta/src/b.ts' },
      { file: folderFile('README.md'), path: 'README.md' },
    ])

    expect(selection.rootName).toBeNull()
    expect(selection.accepted.map((file) => file.path)).toEqual([
      'alpha/src/a.ts',
      'beta/src/b.ts',
      'README.md',
    ])
  })

  test('cannot disguise a secret by mixing it with a different root', () => {
    const selection = prepareLocalImport([
      { file: folderFile('site/src/index.ts'), path: 'site/src/index.ts' },
      { file: folderFile('x/.env', 'SECRET=blocked'), path: 'x/.env' },
    ])

    expect(selection.rootName).toBeNull()
    expect(selection.accepted.map((file) => file.path)).toEqual([
      'site/src/index.ts',
    ])
    expect(selection.skipped).toEqual(['x/.env'])
  })

  test('extracts one ZIP, strips its shared root, and excludes secrets', async () => {
    const bytes = zipSync({
      'site/src/index.ts': strToU8('export const ready = true'),
      'site/.env': strToU8('SECRET=never-upload'),
      'site/node_modules/pkg/index.js': strToU8('ignored'),
    })
    const archive = new File([new Uint8Array(bytes).buffer], 'site.zip', {
      type: 'application/zip',
    })

    const selection = await prepareWorkspaceImport([
      { file: archive, path: archive.name },
    ])

    expect(selection.sourceKind).toBe('zip')
    expect(selection.sourceLabel).toBe('site.zip')
    expect(selection.rootName).toBe('site')
    expect(selection.accepted.map((file) => file.path)).toEqual([
      'src/index.ts',
    ])
    expect(await selection.accepted[0].file.text()).toBe(
      'export const ready = true'
    )
    expect(selection.skipped.sort()).toEqual([
      'site/.env',
      'site/node_modules/pkg/index.js',
    ])
  })

  test('keeps ZIP assets when importing a set of files', async () => {
    const archiveBytes = zipSync({ 'app.ts': strToU8('') })
    const archive = new File([new Uint8Array(archiveBytes).buffer], 'app.zip')

    const selection = await prepareWorkspaceImport([
      { file: archive, path: 'fixtures/app.zip' },
      { file: folderFile('README.md'), path: 'README.md' },
    ])

    expect(selection.sourceKind).toBe('files')
    expect(selection.accepted.map((file) => file.path)).toEqual([
      'fixtures/app.zip',
      'README.md',
    ])
  })

  test('keeps a sole ZIP nested in a dropped folder as a project asset', async () => {
    const archiveBytes = zipSync({ 'app.ts': strToU8('') })
    const archive = new File([new Uint8Array(archiveBytes).buffer], 'app.zip')

    const selection = await prepareWorkspaceImport([
      { file: archive, path: 'fixtures/app.zip' },
    ])

    expect(selection.sourceKind).toBe('files')
    expect(selection.rootName).toBe('fixtures')
    expect(selection.accepted.map((file) => file.path)).toEqual(['app.zip'])
  })

  test('rejects traversal paths from ZIP archives', async () => {
    const archive = new File(
      [
        new Uint8Array(zipSync({ '../outside.txt': strToU8('blocked') }))
          .buffer,
      ],
      'unsafe.zip'
    )

    await expect(
      prepareWorkspaceImport([{ file: archive, path: archive.name }])
    ).rejects.toThrow('unsafe path')
  })

  test('rejects files named ZIP that do not contain an archive', async () => {
    const archive = new File(['not a zip'], 'broken.zip')

    await expect(
      prepareWorkspaceImport([{ file: archive, path: archive.name }])
    ).rejects.toThrow()
  })

  test('recognizes ZIP content types when the filename has no extension', async () => {
    const bytes = zipSync({ 'index.ts': strToU8('export {}') })
    const archive = new File([new Uint8Array(bytes).buffer], 'workspace', {
      type: 'application/zip',
    })

    const selection = await prepareWorkspaceImport([
      { file: archive, path: archive.name },
    ])

    expect(selection.sourceKind).toBe('zip')
    expect(selection.accepted.map((file) => file.path)).toEqual(['index.ts'])
  })

  test('terminates ZIP extraction when the selection is cancelled', async () => {
    const bytes = zipSync({
      'site/index.ts': strToU8('export const cancelled = true'),
    })
    const archive = new File([new Uint8Array(bytes).buffer], 'site.zip')
    const controller = new AbortController()

    const pending = prepareWorkspaceImport(
      [{ file: archive, path: archive.name }],
      { signal: controller.signal }
    )
    controller.abort()

    await expect(pending).rejects.toThrow()
  })

  test('ignores macOS ZIP metadata before detecting the project root', async () => {
    const bytes = zipSync({
      'site/package.json': strToU8('{}'),
      '__MACOSX/site/._package.json': strToU8('metadata'),
    })
    const archive = new File([new Uint8Array(bytes).buffer], 'site.zip')

    const selection = await prepareWorkspaceImport([
      { file: archive, path: archive.name },
    ])

    expect(selection.rootName).toBe('site')
    expect(selection.accepted.map((file) => file.path)).toEqual([
      'package.json',
    ])
    expect(selection.skipped).toContain('__MACOSX/site/._package.json')
  })

  test('rejects oversized ZIP directories before extracting entries', async () => {
    const entries = Object.fromEntries(
      Array.from({ length: 5_001 }, (_, index) => [
        `files/${index}.txt`,
        new Uint8Array(),
      ])
    )
    const bytes = zipSync(entries)
    const archive = new File([new Uint8Array(bytes).buffer], 'too-many.zip')

    await expect(
      prepareWorkspaceImport([{ file: archive, path: archive.name }])
    ).rejects.toThrow('5,000 entry import limit')
  })

  test('skips files that cannot fit in one bounded write request', () => {
    const tooLarge = new File(
      [new Uint8Array(MAX_LOCAL_IMPORT_FILE_BYTES + 1)],
      'large.bin'
    )

    expect(() => prepareLocalImport([tooLarge])).toThrow(
      'contains no importable files'
    )
  })

  test('skips paths longer than the backend import limit', () => {
    const longPath = `${'a'.repeat(513)}.txt`

    expect(() =>
      prepareLocalImport([{ file: folderFile('x'), path: longPath }])
    ).toThrow('contains no importable files')
  })

  test('validates path length after removing a long selected root', () => {
    const root = 'wrapper'.repeat(40)
    const relativePath = `${'a'.repeat(300)}.txt`
    const selection = prepareLocalImport([
      { file: folderFile('x'), path: `${root}/${relativePath}` },
    ])

    expect(selection.rootName).toBe(root)
    expect(selection.accepted.map((file) => file.path)).toEqual([relativePath])
  })
})
