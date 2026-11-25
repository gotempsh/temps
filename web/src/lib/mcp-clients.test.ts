// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  MCP_APPS,
  MCP_KEY_ROLES,
  mcpAddCommand,
  mcpAppSnippet,
  mcpEndpointUrl,
  mcpKeyName,
} from './mcp-clients'

const ORIGIN = 'http://localhost:3014'
const KEY = 'tk_abc12_secret'

describe('MCP apps', () => {
  test('the endpoint always lists the write tools; the key decides', () => {
    expect(mcpEndpointUrl('http://localhost:3014/welcome?x=1')).toBe(
      'http://localhost:3014/mcp?write=1'
    )
    expect(MCP_KEY_ROLES).toEqual({ changes: 'user', read: 'reader' })
  })

  test('each app gets its own settings shape, as temps mcp add writes it', () => {
    const parsed = (id: (typeof MCP_APPS)[number]['id']) =>
      JSON.parse(mcpAppSnippet(id, ORIGIN, KEY))
    const headers = { Authorization: `Bearer ${KEY}` }
    const url = 'http://localhost:3014/mcp?write=1'
    expect(parsed('cursor')).toEqual({
      mcpServers: { temps: { url, headers } },
    })
    expect(parsed('vscode')).toEqual({
      servers: { temps: { type: 'http', url, headers } },
    })
    expect(parsed('windsurf')).toEqual({
      mcpServers: { temps: { serverUrl: url, headers } },
    })
    expect(parsed('zed')).toEqual({
      context_servers: { temps: { enabled: true, url, headers } },
    })
  })

  test('the CLI command matches the access chosen and carries no key', () => {
    expect(mcpAddCommand('cursor', ORIGIN, 'changes')).toBe(
      'bunx @temps-sdk/cli mcp add cursor --url http://localhost:3014 --write -y'
    )
    expect(mcpAddCommand('zed', ORIGIN, 'read')).toBe(
      'bunx @temps-sdk/cli mcp add zed --url http://localhost:3014 -y'
    )
    expect(mcpAddCommand('vscode', ORIGIN, 'changes')).not.toContain('tk_')
  })

  test('with a folder to deploy from, the app runs Temps locally with write access', () => {
    expect(mcpAddCommand('zed', ORIGIN, 'read', '~/code')).toBe(
      'bunx @temps-sdk/cli mcp add zed --url http://localhost:3014 --write --deploy-folder ~/code -y'
    )
  })

  test('key names are MCP client keys, unique to the second', () => {
    expect(mcpKeyName('cursor', new Date('2026-11-25T10:31:05.123Z'))).toBe(
      'mcp-cursor-2026-11-25T10-31-05Z'
    )
  })
})
