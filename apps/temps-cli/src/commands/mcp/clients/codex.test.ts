// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { codexAddArgs, parseCodexMcpGetOutput, TOKEN_ENV_VAR } from './codex.js'

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

describe('codexAddArgs', () => {
  test('over HTTP, config.toml names the variable that holds the token, not the token', () => {
    const args = codexAddArgs({ url: 'https://temps.example.com/mcp', apiKey: 'k' })
    expect(args).toEqual(['mcp', 'add', 'temps', '--url', 'https://temps.example.com/mcp', '--bearer-token-env-var', TOKEN_ENV_VAR])
  })

  test('with deploy folders, the bridge over stdio with the key in its environment', () => {
    expect(
      codexAddArgs({
        url: 'https://temps.example.com/mcp?write=1',
        apiKey: 'k',
        bridge: { folders: ['/srv/apps'], packageSpec: '@temps-sdk/cli@1.2.3' },
      }),
    ).toEqual([
      'mcp',
      'add',
      'temps',
      '--env',
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
    const output = 'temps\n  transport: stdio\n  command: npx\n  args: -y @temps-sdk/cli@1.2.3 mcp bridge --url http://localhost:3014 --folder /srv/apps\n'
    expect(parseCodexMcpGetOutput(output)).toBe('http://localhost:3014/mcp?write=1')
  })
})
