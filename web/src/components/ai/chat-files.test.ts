// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  baseName,
  changeBadge,
  countChanges,
  formatFileSize,
  languageForPath,
  relativeTo,
} from './chat-files'

describe('languageForPath', () => {
  test('maps common extensions to Monaco languages', () => {
    expect(languageForPath('/w/src/app.tsx')).toBe('typescript')
    expect(languageForPath('/w/server.mjs')).toBe('javascript')
    expect(languageForPath('/w/package.json')).toBe('json')
    expect(languageForPath('/w/main.rs')).toBe('rust')
    expect(languageForPath('/w/README.MD')).toBe('markdown')
    expect(languageForPath('/w/compose.yml')).toBe('yaml')
  })

  test('recognises Dockerfiles by name', () => {
    expect(languageForPath('/w/Dockerfile')).toBe('dockerfile')
    expect(languageForPath('/w/Dockerfile.dev')).toBe('dockerfile')
  })

  test('falls back to plain text', () => {
    expect(languageForPath('/w/LICENSE')).toBe('plaintext')
    expect(languageForPath('/w/.gitignore')).toBe('plaintext')
    expect(languageForPath('/w/data.weird')).toBe('plaintext')
  })
})

test('baseName takes the last segment', () => {
  expect(baseName('/home/temps/workspace/src/index.ts')).toBe('index.ts')
  expect(baseName('/home/temps/workspace/')).toBe('workspace')
  expect(baseName('file.txt')).toBe('file.txt')
})

test('relativeTo strips the root only on a segment boundary', () => {
  expect(relativeTo('/w', '/w/src/a.ts')).toBe('src/a.ts')
  expect(relativeTo('/w/', '/w/src/a.ts')).toBe('src/a.ts')
  expect(relativeTo('/w', '/w')).toBe('.')
  expect(relativeTo('/w', '/work/a.ts')).toBe('/work/a.ts')
})

test('every change kind has a badge', () => {
  expect(changeBadge('modified').letter).toBe('M')
  expect(changeBadge('untracked').label).toBe('New file')
  expect(changeBadge('conflicted').letter).toBe('!')
})

test('countChanges sums files across repositories', () => {
  expect(countChanges([])).toBe(0)
  expect(countChanges([{ files: [1, 2] }, { files: [3] }])).toBe(3)
})

test('formatFileSize picks a readable unit', () => {
  expect(formatFileSize(512)).toBe('512 B')
  expect(formatFileSize(3482)).toBe('3.4 KB')
  expect(formatFileSize(1258291)).toBe('1.2 MB')
})
