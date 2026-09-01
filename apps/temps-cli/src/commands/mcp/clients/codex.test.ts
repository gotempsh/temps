// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { parseCodexMcpGetOutput } from './codex.js'

describe('parseCodexMcpGetOutput', () => {
  test('extracts the URL from a well-formed mcp get entry', () => {
    const output = 'temps\n  transport: streamable-http\n  url: http://localhost:3000/mcp\n  bearer_token_env_var: TEMPS_MCP_AUTH_HEADER\n'
    expect(parseCodexMcpGetOutput(output)).toBe('http://localhost:3000/mcp')
  })

  test('extracts a URL with query params', () => {
    const output = 'url: https://temps.example.com/mcp?groups=deployments&write=1\n'
    expect(parseCodexMcpGetOutput(output)).toBe('https://temps.example.com/mcp?groups=deployments&write=1')
  })

  test('tolerates leading whitespace before the url: line', () => {
    const output = 'temps\n    url: http://localhost:3000/mcp\n    transport: streamable-http\n'
    expect(parseCodexMcpGetOutput(output)).toBe('http://localhost:3000/mcp')
  })

  test('returns null when there is no url: line', () => {
    const output = 'temps\n  transport: stdio\n  command: npx some-other-server\n'
    expect(parseCodexMcpGetOutput(output)).toBeNull()
  })

  test('returns null for empty output', () => {
    expect(parseCodexMcpGetOutput('')).toBeNull()
  })

  test('does not match an unrelated line merely containing "url:" mid-sentence', () => {
    const output = 'notes: the url: http://ignored.example.com appears in a comment\n'
    expect(parseCodexMcpGetOutput(output)).toBeNull()
  })
})
