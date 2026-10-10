// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  classifyDropPath,
  formatDetectedProjectLabel,
  htmlRootCandidates,
  normalizeDropFiles,
  partitionDropFiles,
  prepareDrop,
  type DropFile,
} from './drop-archive'

function dropped(path: string, contents = path): DropFile {
  const parts = path.split('/')
  return { file: new File([contents], parts[parts.length - 1]), path }
}

describe('drop archive preparation', () => {
  test('hides the internal dot path for root project candidates', () => {
    expect(formatDetectedProjectLabel('Static Site', '.')).toBe('Static Site')
    expect(formatDetectedProjectLabel('Vite', 'apps/web')).toBe(
      'Vite · apps/web'
    )
  })

  test('removes the selected folder name but preserves its tree', () => {
    const files = normalizeDropFiles([
      dropped('portfolio/index.html'),
      dropped('portfolio/assets/site.css'),
    ])

    expect(files.map(({ path }) => path)).toEqual([
      'index.html',
      'assets/site.css',
    ])
  })

  test('ignores operating-system metadata', () => {
    const files = normalizeDropFiles([
      dropped('site/index.html'),
      dropped('site/.DS_Store'),
      dropped('site/__MACOSX/._index.html'),
    ])

    expect(files.map(({ path }) => path)).toEqual(['index.html'])
  })

  test('excludes secrets, git metadata, and dependency trees', () => {
    const files = normalizeDropFiles([
      dropped('site/index.html'),
      dropped('site/.env', 'TOKEN=secret'),
      dropped('site/.env.local', 'TOKEN=secret'),
      dropped('site/server.pem', 'secret'),
      dropped('site/.git/config'),
      dropped('site/node_modules/pkg/index.js'),
    ])

    expect(files.map(({ path }) => path)).toEqual(['index.html'])
  })

  test('keeps environment templates, as a ZIP of the same folder does', () => {
    const files = normalizeDropFiles([
      dropped('app/Cargo.toml'),
      dropped('app/.env.example', 'PORT=8080'),
      dropped('app/.env.sample'),
      dropped('app/.env.template'),
      dropped('app/.env.dist'),
      dropped('app/.env', 'TOKEN=secret'),
    ])

    expect(files.map(({ path }) => path)).toEqual([
      'Cargo.toml',
      '.env.example',
      '.env.sample',
      '.env.template',
      '.env.dist',
    ])
  })

  test('reports what it left out, one line per directory', () => {
    const { leftOut } = partitionDropFiles([
      dropped('app/index.js'),
      dropped('app/.env', 'TOKEN=secret'),
      dropped('app/node_modules/a/index.js'),
      dropped('app/node_modules/b/index.js'),
      dropped('app/.git/HEAD'),
      dropped('app/certs/server.key', 'secret'),
    ])

    expect(leftOut).toEqual([
      { path: '.env', reason: 'environment_file', files: 1 },
      { path: '.git/', reason: 'version_control', files: 1 },
      { path: 'certs/server.key', reason: 'private_key', files: 1 },
      { path: 'node_modules/', reason: 'dependencies', files: 2 },
    ])
  })

  test('classifies paths with the same rule as the server', () => {
    expect(classifyDropPath('apps/api/.env.production.example')).toEqual({
      kind: 'keep',
    })
    expect(classifyDropPath('node_modules/x/test/cert.pem')).toEqual({
      kind: 'skip',
      reason: 'dependencies',
      reportedPath: 'node_modules/',
    })
    expect(classifyDropPath('config/.env.local')).toEqual({
      kind: 'skip',
      reason: 'environment_file',
      reportedPath: 'config/.env.local',
    })
  })

  test('finds root page choices when index.html is absent', () => {
    expect(
      htmlRootCandidates([
        dropped('site/about.html'),
        dropped('site/docs/start.html'),
      ])
    ).toEqual(['about.html', 'docs/start.html'])
  })

  test('packages a single HTML file as a valid zip with an index redirect', async () => {
    const prepared = await prepareDrop([
      dropped('landing.html', '<h1>Hello</h1>'),
    ])
    const bytes = new Uint8Array(await prepared.file.arrayBuffer())
    const archiveText = new TextDecoder().decode(bytes)

    expect(new DataView(bytes.buffer).getUint32(0, true)).toBe(0x04034b50)
    expect(archiveText).toContain('landing.html')
    expect(archiveText).toContain('index.html')
    expect(prepared.rootPage).toBe('landing.html')
  })

  test('passes an existing archive through without copying it', async () => {
    const archive = new File(['zip'], 'site.zip', { type: 'application/zip' })
    const prepared = await prepareDrop([{ file: archive, path: archive.name }])

    expect(prepared.file).toBe(archive)
    expect(prepared.rootPage).toBeNull()
  })

  test('rejects traversal paths', () => {
    expect(() => normalizeDropFiles([dropped('../index.html')])).toThrow(
      'Unsafe path'
    )
  })
})
