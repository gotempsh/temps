// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, test } from 'bun:test'
import { mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { bridgeKeyFilePath, type McpServerEntry } from './base.js'
import { CodexAdapter, codexAddArgs, codexHome, parseCodexMcpGetOutput, TOKEN_ENV_VAR } from './codex.js'

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

  test('with deploy folders, the bridge over stdio, naming the private key file and never holding the key', () => {
    const args = codexAddArgs({
      url: 'https://temps.example.com/mcp?write=1',
      apiKey: 'tk_secret-bridge-key',
      bridge: { folders: ['/srv/apps'], packageSpec: '@temps-sdk/cli@1.2.3' },
    })
    // These are codex's arguments, readable by other users in the process list.
    expect(args.join(' ')).not.toContain('tk_secret-bridge-key')
    expect(args).toEqual([
      'mcp',
      'add',
      'temps',
      '--env',
      `TEMPS_API_KEY_FILE=${bridgeKeyFilePath('codex', 'https://temps.example.com', codexHome())}`,
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

/**
 * A stand-in for the app's CLI: `mcp get` prints the saved entry, `mcp add`
 * refuses a name that exists (as `claude` does) unless REPLACE is set (as
 * `codex` does), `mcp remove` deletes it. State lives in STATE_DIR.
 */
function fakeCli(dir: string): string {
  const file = join(dir, 'fake-cli')
  writeFileSync(
    file,
    [
      '#!/bin/sh',
      'entry="$STATE_DIR/entry"',
      'case "$2" in',
      '  get) [ -f "$entry" ] && cat "$entry" && exit 0; echo "No MCP server found with name: temps" >&2; exit 1;;',
      '  add) if [ -f "$entry" ] && [ -z "$REPLACE" ]; then echo "MCP server temps already exists in user config" >&2; exit 1; fi',
      '       mkdir -p "$STATE_DIR"; echo "Args: $*" > "$entry"; exit 0;;',
      '  remove) [ -f "$entry" ] || { echo "No MCP server found with name: temps" >&2; exit 1; }; rm "$entry"; exit 0;;',
      'esac',
      'exit 2',
    ].join('\n'),
    { mode: 0o755 },
  )
  return file
}

const bridgeTo = (origin: string, apiKey: string): McpServerEntry => ({
  url: `${origin}/mcp?write=1`,
  apiKey,
  bridge: { folders: ['/srv/apps'], packageSpec: '@temps-sdk/cli@1.2.3' },
})

/** The key file an installed entry reads, from the entry the stand-in saved. */
function installedKeyFile(stateDir: string): string {
  const match = readFileSync(join(stateDir, 'entry'), 'utf8').match(/TEMPS_API_KEY_FILE=(\S+)/)
  if (!match?.[1]) throw new Error('the saved entry names no key file')
  return match[1]
}

describe('CodexAdapter with a bridge in two CODEX_HOME profiles', () => {
  let dir: string
  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), 'temps-codex-adapter-'))
  })
  afterEach(() => rmSync(dir, { recursive: true, force: true }))

  test('each profile keeps its own instance\'s key, and removing one leaves the other working', async () => {
    const binary = fakeCli(dir)
    const profile = (name: string) => {
      const home = join(dir, name)
      return { home, env: { PATH: process.env.PATH, CODEX_HOME: home, STATE_DIR: home, REPLACE: '1' } }
    }
    const one = profile('codex-one')
    const two = profile('codex-two')
    const first = new CodexAdapter({ binary, home: dir, env: one.env })
    const second = new CodexAdapter({ binary, home: dir, env: two.env })
    expect(await first.addServer(bridgeTo('https://a.example.test', 'key-a'))).toEqual({ success: true })
    expect(await second.addServer(bridgeTo('https://b.example.test', 'key-b'))).toEqual({ success: true })

    const fileOne = installedKeyFile(one.home)
    const fileTwo = installedKeyFile(two.home)
    expect(fileOne).not.toBe(fileTwo)
    // The first profile still sends A's key to A.
    expect(readFileSync(fileOne, 'utf8').trim()).toBe('key-a')
    expect(readFileSync(fileTwo, 'utf8').trim()).toBe('key-b')

    expect((await first.removeServer()).success).toBe(true)
    expect(() => readFileSync(fileOne)).toThrow()
    expect(readFileSync(fileTwo, 'utf8').trim()).toBe('key-b')
  })
})
