// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { formatBytes, parseSourceFileExtensions, matchesSourceFileFilter } from './index.js'

describe('formatBytes', () => {
  test('renders sub-KB sizes as whole bytes', () => {
    expect(formatBytes(0)).toBe('0 B')
    expect(formatBytes(512)).toBe('512 B')
    expect(formatBytes(1023)).toBe('1023 B')
  })

  test('renders KB with one decimal', () => {
    expect(formatBytes(1024)).toBe('1.0 KB')
    expect(formatBytes(2048)).toBe('2.0 KB')
  })

  test('boundary just under 1 MB is still reported in KB', () => {
    // The KB/MB threshold is an exclusive `< 1024 * 1024`, so a size one byte
    // short of a full MB reads as "1024.0 KB" rather than "1.00 MB".
    expect(formatBytes(1024 * 1024 - 1)).toBe('1024.0 KB')
  })

  test('renders MB with two decimals at and above the 1 MB boundary', () => {
    expect(formatBytes(1024 * 1024)).toBe('1.00 MB')
    expect(formatBytes(1024 * 1024 * 3)).toBe('3.00 MB')
  })
})

describe('parseSourceFileExtensions', () => {
  test('falls back to the built-in language list when --ext is omitted', () => {
    const exts = parseSourceFileExtensions(undefined)
    expect(exts.has('go')).toBe(true)
    expect(exts.has('rs')).toBe(true)
    expect(exts.has('ts')).toBe(true)
  })

  test('normalizes a custom CSV: trims, lowercases, and strips a leading dot', () => {
    const exts = parseSourceFileExtensions(' .PY , Rb ,go')
    expect(exts).toEqual(new Set(['py', 'rb', 'go']))
  })

  test('drops empty entries from a trailing or doubled comma', () => {
    const exts = parseSourceFileExtensions('go,,rs,')
    expect(exts).toEqual(new Set(['go', 'rs']))
  })
})

describe('matchesSourceFileFilter', () => {
  const exts = new Set(['go', 'ts'])

  test('accepts a file with a matching extension', () => {
    expect(matchesSourceFileFilter('internal/gateway/main.go', exts)).toBe(true)
  })

  test('rejects a file with a non-matching extension', () => {
    expect(matchesSourceFileFilter('README.md', exts)).toBe(false)
  })

  test('rejects files under a skipped directory at any depth', () => {
    expect(matchesSourceFileFilter('node_modules/pkg/index.go', exts)).toBe(false)
    expect(matchesSourceFileFilter('a/b/vendor/c/main.go', exts)).toBe(false)
    expect(matchesSourceFileFilter('.git/hooks/pre-commit.go', exts)).toBe(false)
  })

  test('matches extension case-insensitively', () => {
    expect(matchesSourceFileFilter('main.GO', exts)).toBe(true)
  })

  test('rejects a file with no extension', () => {
    expect(matchesSourceFileFilter('Makefile', exts)).toBe(false)
  })
})
