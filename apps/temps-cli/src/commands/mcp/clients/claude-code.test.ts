// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { parseClaudeCodeMcpGetOutput } from './claude-code.js'

describe('parseClaudeCodeMcpGetOutput', () => {
  test('extracts the URL from a well-formed mcp get entry', () => {
    const output = 'temps\n  Scope: User\n  Status: ✓ Connected\n  Type: http\n  URL: http://localhost:3000/mcp\n'
    expect(parseClaudeCodeMcpGetOutput(output)).toBe('http://localhost:3000/mcp')
  })

  test('extracts a URL with query params', () => {
    const output = 'URL: https://temps.example.com/mcp?groups=platform&write=1\n'
    expect(parseClaudeCodeMcpGetOutput(output)).toBe('https://temps.example.com/mcp?groups=platform&write=1')
  })

  test('tolerates leading whitespace before the URL: line', () => {
    const output = 'temps\n    URL: http://localhost:3000/mcp\n    Type: http\n'
    expect(parseClaudeCodeMcpGetOutput(output)).toBe('http://localhost:3000/mcp')
  })

  test('returns null when there is no URL: line', () => {
    const output = 'temps\n  Scope: User\n  Type: stdio\n  Command: npx some-other-server\n'
    expect(parseClaudeCodeMcpGetOutput(output)).toBeNull()
  })

  test('returns null for empty output', () => {
    expect(parseClaudeCodeMcpGetOutput('')).toBeNull()
  })

  test('is case-sensitive on the "URL:" label -- a lowercase "url:" line does not match', () => {
    const output = 'url: http://lowercase-should-not-match.example.com\n'
    expect(parseClaudeCodeMcpGetOutput(output)).toBeNull()
  })

  test('takes the first URL: line when the output has multiple (defensive; real CLI only emits one)', () => {
    const output = 'URL: http://first.example.com/mcp\nURL: http://second.example.com/mcp\n'
    expect(parseClaudeCodeMcpGetOutput(output)).toBe('http://first.example.com/mcp')
  })
})
