// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, test } from 'bun:test'
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { bridgeKeyFilePath, localBridgeCommand, type McpServerEntry } from './base.js'
import { ClaudeCodeAdapter, claudeCodeAddArgs, claudeCodeEntryMatches, parseClaudeCodeMcpGetOutput } from './claude-code.js'

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
        apiKey: 'tk_secret-bridge-key',
        bridge: { folders: ['/srv/apps'], packageSpec: '@temps-sdk/cli@1.2.3' },
      }),
    ).toEqual([
      'mcp',
      'add',
      '--scope',
      'user',
      'temps',
      '-e',
      `TEMPS_API_KEY_FILE=${bridgeKeyFilePath('claude-code', 'https://temps.example.com')}`,
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
    expect(
      claudeCodeAddArgs({
        url: 'https://temps.example.com/mcp?write=1',
        apiKey: 'tk_secret-bridge-key',
        bridge: { folders: ['/srv/apps'], packageSpec: '@temps-sdk/cli@1.2.3' },
      }).join(' '),
    ).not.toContain('tk_secret-bridge-key')
  })

  test('status reads a bridge entry back as read/write', () => {
    const output =
      'temps:\n  Scope: User config\n  Type: stdio\n  Command: npx\n  Args: -y @temps-sdk/cli@1.2.3 mcp bridge --url https://temps.example.com --folder /srv/apps\n'
    expect(parseClaudeCodeMcpGetOutput(output)).toBe('https://temps.example.com/mcp?write=1')
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

/** The arguments Claude Code saves for an entry's bridge. */
const savedArgs = (entry: McpServerEntry): string[] => localBridgeCommand(entry, entry.bridge!).args

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

describe('ClaudeCodeAdapter.addServer with a bridge', () => {
  let dir: string
  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), 'temps-claude-adapter-'))
  })
  afterEach(() => rmSync(dir, { recursive: true, force: true }))

  test('a refused install for another instance keeps the installed entry\'s key, and says why', async () => {
    const env = { PATH: process.env.PATH, STATE_DIR: join(dir, 'state') }
    const adapter = new ClaudeCodeAdapter({ binary: fakeCli(dir), home: dir, env })
    expect(await adapter.addServer(bridgeTo('https://a.example.test', 'key-a'))).toEqual({ success: true })
    const fileA = installedKeyFile(env.STATE_DIR)
    expect(readFileSync(fileA, 'utf8').trim()).toBe('key-a')

    const result = await adapter.addServer(bridgeTo('https://b.example.test', 'key-b'))
    expect(result.success).toBe(false)
    expect(result.reason).toContain('already has a temps server for https://a.example.test')
    // The entry still reads A's file, which still holds A's key, and B's key
    // is nowhere on disk.
    expect(installedKeyFile(env.STATE_DIR)).toBe(fileA)
    expect(readFileSync(fileA, 'utf8').trim()).toBe('key-a')
    const keys = readdirSync(join(dir, '.temps', 'mcp')).map((name) => readFileSync(join(dir, '.temps', 'mcp', name), 'utf8'))
    expect(keys.join('')).not.toContain('key-b')
  })

  test('a refused install is already installed only when the setup is the same, not merely the instance', async () => {
    const env = { PATH: process.env.PATH, STATE_DIR: join(dir, 'state') }
    const adapter = new ClaudeCodeAdapter({ binary: fakeCli(dir), home: dir, env })
    // The ordinary HTTP entry first, as `claude mcp get` prints it.
    mkdirSync(env.STATE_DIR, { recursive: true })
    writeFileSync(
      join(env.STATE_DIR, 'entry'),
      'temps:\n  Scope: User config\n  Type: http\n  URL: https://a.example.test/mcp\n',
    )
    // Asked again with a folder: Claude keeps the HTTP entry, which cannot
    // deploy folders, so this is no success.
    const withFolder = await adapter.addServer(bridgeTo('https://a.example.test', 'key-a2'))
    expect(withFolder.success).toBe(false)
    expect(withFolder.reason).toContain('set up differently')
    expect(withFolder.reason).toContain('temps mcp remove claude-code')

    // A bridge asked for other folders is not the installed one either.
    expect((await adapter.removeServer()).success).toBe(true)
    await adapter.addServer(bridgeTo('https://a.example.test', 'key-a'))
    const other = await adapter.addServer({
      ...bridgeTo('https://a.example.test', 'key-a3'),
      bridge: { folders: ['/srv/apps', '/srv/more'], packageSpec: '@temps-sdk/cli@1.2.3' },
    })
    expect(other.success).toBe(false)
    expect(other.reason).toContain('set up differently')
  })

  test('a bridge for a folder with a space in its name is recognised as installed from the saved arguments', async () => {
    const env = { PATH: process.env.PATH, STATE_DIR: join(dir, 'state') }
    const adapter = new ClaudeCodeAdapter({ binary: fakeCli(dir), home: dir, env })
    const entry = {
      ...bridgeTo('https://a.example.test', 'key-a'),
      bridge: { folders: ['/srv/My Projects'], packageSpec: '@temps-sdk/cli@1.2.3' },
    }
    expect(await adapter.addServer(entry)).toEqual({ success: true })
    // Without Claude Code's saved configuration the printed line is all
    // there is, and it cannot tell this folder from others: not installed.
    expect((await adapter.addServer({ ...entry, apiKey: 'key-a2' })).success).toBe(false)
    // With it, as Claude Code keeps it, the arguments are exact.
    writeFileSync(
      join(dir, '.claude.json'),
      JSON.stringify({ mcpServers: { temps: { type: 'stdio', command: 'npx', args: savedArgs(entry) } } }),
    )
    expect(await adapter.addServer({ ...entry, apiKey: 'key-a3' })).toEqual({
      success: true,
      alreadyInstalled: true,
    })
  })

  test('different folder lists that print the same line are never taken for each other', () => {
    const two = {
      ...bridgeTo('https://a.example.test', 'key-a'),
      bridge: { folders: ['/srv/a', '/srv/b'], packageSpec: '@temps-sdk/cli@1.2.3' },
    }
    const one = {
      ...two,
      bridge: { folders: ['/srv/a --folder /srv/b'], packageSpec: '@temps-sdk/cli@1.2.3' },
    }
    // Both print this line.
    const printed =
      'temps:\n  Type: stdio\n  Command: npx\n  Args: -y @temps-sdk/cli@1.2.3 mcp bridge --url https://a.example.test --folder /srv/a --folder /srv/b\n'
    // From the line alone: the two folders match, the one with spaces is
    // unverifiable and so not installed.
    expect(claudeCodeEntryMatches(printed, two)).toBe(true)
    expect(claudeCodeEntryMatches(printed, one)).toBe(false)
    // From the saved arguments, each matches only itself.
    expect(claudeCodeEntryMatches(printed, two, savedArgs(two))).toBe(true)
    expect(claudeCodeEntryMatches(printed, one, savedArgs(two))).toBe(false)
    expect(claudeCodeEntryMatches(printed, two, savedArgs(one))).toBe(false)
    expect(claudeCodeEntryMatches(printed, one, savedArgs(one))).toBe(true)
  })

  test('a refused install for the same instance is already installed, and leaves the key the entry sends', async () => {
    const env = { PATH: process.env.PATH, STATE_DIR: join(dir, 'state') }
    const adapter = new ClaudeCodeAdapter({ binary: fakeCli(dir), home: dir, env })
    await adapter.addServer(bridgeTo('https://a.example.test', 'key-a'))
    expect(await adapter.addServer(bridgeTo('https://a.example.test', 'key-a2'))).toEqual({
      success: true,
      alreadyInstalled: true,
    })
    expect(readFileSync(installedKeyFile(env.STATE_DIR), 'utf8').trim()).toBe('key-a')
    expect((await adapter.removeServer()).success).toBe(true)
    expect(readdirSync(join(dir, '.temps', 'mcp'))).toEqual([])
  })
})
