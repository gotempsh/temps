// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

// Plugin mutations (install, update, permissions, reload, uninstall) carry no
// `onError` of their own, so the app-wide default in App.tsx already toasts
// "<problem title>" + "<problem detail>" when they fail. A second
// `toast.error` in the call site's catch block showed the failure twice
// (title-only and title+detail). Failures must be reported in one place.
const SOURCES = [
  'RepositoryInstall.tsx',
  'RepositoryUpdate.tsx',
  'PluginPermissionsDialog.tsx',
  '../../pages/settings/PluginsPage.tsx',
]

// Bodies of every `catch (error) { ... }` block, found by brace matching.
function catchBlocks(source: string): string[] {
  const blocks: string[] = []
  const marker = /catch\s*\(\s*\w+\s*\)\s*\{/g
  for (let m = marker.exec(source); m; m = marker.exec(source)) {
    let depth = 1
    let i = marker.lastIndex
    while (i < source.length && depth > 0) {
      if (source[i] === '{') depth++
      else if (source[i] === '}') depth--
      i++
    }
    blocks.push(source.slice(marker.lastIndex, i - 1))
  }
  return blocks
}

describe('plugin mutation failure toasts', () => {
  test('catchBlocks extracts nested catch bodies', () => {
    const blocks = catchBlocks('try { a() } catch (e) { if (x) { y() } z() }')
    expect(blocks).toEqual([' if (x) { y() } z() '])
  })

  for (const file of SOURCES) {
    test(`${file} leaves failure toasts to the global mutation handler`, () => {
      const source = readFileSync(join(import.meta.dir, file), 'utf8')
      const blocks = catchBlocks(source)
      expect(blocks.length).toBeGreaterThan(0)
      for (const block of blocks) {
        expect(block).not.toContain('toast.error')
      }
    })
  }
})
