// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  BULK_ERROR_GROUP_LIMIT,
  buildAssigneeBody,
  buildIgnoreConditions,
  checkAssigneeFilter,
  checkInNextRelease,
  formatBytes,
  matchesSourceFileFilter,
  parseGroupIds,
  parseSourceFileExtensions,
} from './index.js'

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

describe('parseGroupIds', () => {
  test('parses a comma-separated list and drops repeats', () => {
    expect(parseGroupIds('12,15,12, 18 ')).toEqual([12, 15, 18])
  })

  test('rejects anything that is not a positive group ID', () => {
    expect(() => parseGroupIds('12,abc')).toThrow('Invalid group ID: abc')
    expect(() => parseGroupIds('0')).toThrow('Invalid group ID: 0')
    expect(() => parseGroupIds('-3')).toThrow('Invalid group ID: -3')
    expect(() => parseGroupIds('1.5')).toThrow('Invalid group ID: 1.5')
  })

  test('rejects an empty selection', () => {
    expect(() => parseGroupIds(' , ')).toThrow('at least one group ID')
  })

  test('rejects more groups than the server accepts in one request', () => {
    const ids = Array.from({ length: BULK_ERROR_GROUP_LIMIT + 1 }, (_, i) => i + 1)
    expect(() => parseGroupIds(ids.join(','))).toThrow(
      `at most ${BULK_ERROR_GROUP_LIMIT} per request`
    )
  })
})

describe('buildIgnoreConditions', () => {
  const now = new Date('2026-01-10T12:00:00.000Z')

  test('no flag is a plain ignore', () => {
    expect(buildIgnoreConditions({}, now)).toBeUndefined()
  })

  test('a duration becomes an absolute time', () => {
    expect(buildIgnoreConditions({ ignoreFor: '7d' }, now)).toEqual({
      until: '2026-01-17T12:00:00.000Z',
    })
    expect(buildIgnoreConditions({ ignoreFor: '30m' }, now)).toEqual({
      until: '2026-01-10T12:30:00.000Z',
    })
  })

  test('counts and users combine', () => {
    expect(buildIgnoreConditions({ ignoreCount: '100', ignoreUsers: '10' }, now)).toEqual({
      event_count: 100,
      user_count: 10,
    })
  })

  test('bad values and contradictory flags are refused', () => {
    expect(() => buildIgnoreConditions({ ignoreFor: 'a week' }, now)).toThrow('--ignore-for')
    expect(() => buildIgnoreConditions({ ignoreCount: '0' }, now)).toThrow('--ignore-count')
    expect(() => buildIgnoreConditions({ ignoreUntil: 'tomorrow' }, now)).toThrow('--ignore-until')
    expect(() =>
      buildIgnoreConditions({ ignoreFor: '1d', ignoreUntil: '2026-02-01T00:00:00Z' }, now)
    ).toThrow('not both')
    expect(() => buildIgnoreConditions({ ignoreForever: true, ignoreUsers: '3' }, now)).toThrow(
      '--ignore-forever'
    )
  })

  test('forever stands alone', () => {
    expect(buildIgnoreConditions({ ignoreForever: true }, now)).toEqual({ forever: true })
  })
})

describe('checkInNextRelease', () => {
  test('goes with a resolve only', () => {
    expect(() => checkInNextRelease(true, true)).not.toThrow()
    expect(() => checkInNextRelease(undefined, false)).not.toThrow()
    expect(() => checkInNextRelease(true, false)).toThrow('--in-next-release')
  })
})

describe('buildAssigneeBody', () => {
  test('takes exactly one target', () => {
    expect(buildAssigneeBody({ userId: '7' }, undefined)).toEqual({ user_id: 7 })
    expect(buildAssigneeBody({ teamId: '3' }, undefined)).toEqual({ team_id: 3 })
    expect(buildAssigneeBody({ me: true }, 12)).toEqual({ user_id: 12 })
    expect(buildAssigneeBody({ none: true }, undefined)).toEqual({})
    expect(() => buildAssigneeBody({}, undefined)).toThrow('exactly one')
    expect(() => buildAssigneeBody({ userId: '7', teamId: '3' }, undefined)).toThrow('exactly one')
    expect(() => buildAssigneeBody({ userId: 'x' }, undefined)).toThrow('positive numeric id')
    expect(() => buildAssigneeBody({ me: true }, undefined)).toThrow('--me')
  })
})

describe('checkAssigneeFilter', () => {
  test('accepts the four forms only', () => {
    for (const ok of ['me', 'none', 'user:4', ' team:9 ']) {
      expect(() => checkAssigneeFilter(ok)).not.toThrow()
    }
    for (const bad of ['', 'you', 'user:', 'team:x']) {
      expect(() => checkAssigneeFilter(bad)).toThrow('--assignee')
    }
  })
})
