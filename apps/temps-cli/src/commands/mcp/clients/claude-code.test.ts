// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { claudeCodeAddArgs, parseClaudeCodeMcpGetOutput } from './claude-code.js'

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

describe('claudeCodeAddArgs', () => {
  test('over HTTP, the endpoint and the key as a header', () => {
    expect(claudeCodeAddArgs({ url: 'https://temps.example.com/mcp?write=1', apiKey: 'k' })).toEqual([
      'mcp',
      'add',
      '--transport',
      'http',
      '--scope',
      'user',
      'temps',
      'https://temps.example.com/mcp?write=1',
      '--header',
      'Authorization: Bearer k',
    ])
  })

  test('with deploy folders, the bridge over stdio: the name before -e, the command after --', () => {
    expect(
      claudeCodeAddArgs({
        url: 'https://temps.example.com/mcp?write=1',
        apiKey: 'k',
        bridge: { folders: ['/srv/apps'], packageSpec: '@temps-sdk/cli@1.2.3' },
      }),
    ).toEqual([
      'mcp',
      'add',
      '--scope',
      'user',
      'temps',
      '-e',
      'TEMPS_API_KEY=k',
      '--',
      'npx',
      '-y',
      '@temps-sdk/cli@1.2.3',
      'mcp',
      'bridge',
      '--url',
      'https://temps.example.com',
      '--folder',
      '/srv/apps',
    ])
  })

  test('status reads a bridge entry back as read/write', () => {
    const output =
      'temps:\n  Scope: User config\n  Type: stdio\n  Command: npx\n  Args: -y @temps-sdk/cli@1.2.3 mcp bridge --url https://temps.example.com --folder /srv/apps\n'
    expect(parseClaudeCodeMcpGetOutput(output)).toBe('https://temps.example.com/mcp?write=1')
  })
})
