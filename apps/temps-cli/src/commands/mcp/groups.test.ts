// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { buildMcpUrl, isValidGroupKey, parseMcpUrl, TOOL_GROUPS } from './groups.js'

describe('isValidGroupKey', () => {
  test('accepts every declared group key', () => {
    for (const group of TOOL_GROUPS) {
      expect(isValidGroupKey(group.key)).toBe(true)
    }
  })

  test('rejects an unknown key', () => {
    expect(isValidGroupKey('not-a-group')).toBe(false)
  })
})

describe('buildMcpUrl', () => {
  test('omits groups and write when all groups selected and write disabled', () => {
    const url = buildMcpUrl('http://localhost:3000', TOOL_GROUPS.map((g) => g.key), false)
    expect(url).toBe('http://localhost:3000/mcp')
  })

  test('includes groups when a subset is selected', () => {
    const url = buildMcpUrl('http://localhost:3000', ['deployments', 'observability'], false)
    expect(url).toBe('http://localhost:3000/mcp?groups=deployments%2Cobservability')
  })

  test('includes write=1 when write is enabled', () => {
    const url = buildMcpUrl('http://localhost:3000', TOOL_GROUPS.map((g) => g.key), true)
    expect(url).toBe('http://localhost:3000/mcp?write=1')
  })

  test('strips a trailing slash from the configured API URL', () => {
    const url = buildMcpUrl('http://localhost:3000/', TOOL_GROUPS.map((g) => g.key), false)
    expect(url).toBe('http://localhost:3000/mcp')
  })

  test('combines groups and write', () => {
    const url = buildMcpUrl('https://temps.example.com', ['platform'], true)
    expect(url).toBe('https://temps.example.com/mcp?groups=platform&write=1')
  })
})

describe('parseMcpUrl', () => {
  test('is the inverse of buildMcpUrl for every combination', () => {
    for (const groups of [TOOL_GROUPS.map((g) => g.key), ['deployments', 'observability'], ['platform']]) {
      for (const write of [true, false]) {
        const url = buildMcpUrl('https://temps.example.com', groups, write)
        expect(parseMcpUrl(url)).toEqual({ groups, write })
      }
    }
  })

  test('defaults to all groups and write=false when the URL has no query params', () => {
    expect(parseMcpUrl('https://temps.example.com/mcp')).toEqual({
      groups: TOOL_GROUPS.map((g) => g.key),
      write: false,
    })
  })

  test('drops unknown group keys from a hand-edited or stale URL', () => {
    const parsed = parseMcpUrl('https://temps.example.com/mcp?groups=platform,not-a-group')
    expect(parsed).toEqual({ groups: ['platform'], write: false })
  })

  test('returns null for an unparsable URL', () => {
    expect(parseMcpUrl('not a url')).toBeNull()
  })
})
