// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, test } from 'bun:test'
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import * as jsonc from 'jsonc-parser'
import {
  bridgeKeyFilePath,
  bridgeUrlFromArgs,
  bridgeUrlFromText,
  JsonConfigMcpClientAdapter,
  localBridgeCommand,
  removeBridgeKeyFile,
  writeBridgeKeyFile,
  type McpServerEntry,
} from './base.js'

class TestAdapter extends JsonConfigMcpClientAdapter {
  readonly id = 'test-client'
  readonly label = 'Test Client'
  constructor(private configPath: string) {
    super()
  }
  protected getConfigPath(): string {
    return this.configPath
  }
  protected getServerPropertyName(): string {
    return 'mcpServers'
  }
  protected buildServerConfig(entry: McpServerEntry): Record<string, unknown> {
    return { url: entry.url, headers: { Authorization: `Bearer ${entry.apiKey}` } }
  }
  protected extractUrl(serverConfig: Record<string, unknown>): string | null {
    return typeof serverConfig.url === 'string' ? serverConfig.url : null
  }
}

describe('JsonConfigMcpClientAdapter', () => {
  let dir: string
  let configPath: string
  let adapter: TestAdapter
  const entry: McpServerEntry = { url: 'http://localhost:3000/mcp', apiKey: 'secret-key' }

  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), 'temps-mcp-client-'))
    configPath = join(dir, 'nested', 'config.json')
    adapter = new TestAdapter(configPath)
  })

  afterEach(() => {
    rmSync(dir, { recursive: true, force: true })
  })

  test('reports not installed when the config file does not exist', async () => {
    expect(await adapter.isServerInstalled()).toBe(false)
  })

  test('creates missing parent directories and writes a fresh config', async () => {
    const result = await adapter.addServer(entry)
    expect(result).toEqual({ success: true })
    expect(await adapter.isServerInstalled()).toBe(true)

    const written = JSON.parse(readFileSync(configPath, 'utf8'))
    expect(written.mcpServers.temps).toEqual({
      url: entry.url,
      headers: { Authorization: 'Bearer secret-key' },
    })
  })

  test('re-adding an identical entry reports alreadyInstalled without rewriting', async () => {
    await adapter.addServer(entry)
    const before = readFileSync(configPath, 'utf8')

    const result = await adapter.addServer(entry)
    expect(result).toEqual({ success: true, alreadyInstalled: true })
    expect(readFileSync(configPath, 'utf8')).toBe(before)
  })

  test('re-adding with a different URL overwrites the entry', async () => {
    await adapter.addServer(entry)
    const result = await adapter.addServer({ ...entry, url: 'http://localhost:3000/mcp?write=1' })
    expect(result).toEqual({ success: true })

    const written = JSON.parse(readFileSync(configPath, 'utf8'))
    expect(written.mcpServers.temps.url).toBe('http://localhost:3000/mcp?write=1')
  })

  test('preserves unrelated existing entries and comments', async () => {
    mkdirSync(join(dir, 'nested'), { recursive: true })
    writeFileSync(
      configPath,
      `{
  // a comment a human wrote
  "mcpServers": {
    "other": { "command": "npx", "args": ["-y", "other-mcp"] }
  }
}
`,
    )
    await adapter.addServer(entry)

    const content = readFileSync(configPath, 'utf8')
    expect(content).toContain('a comment a human wrote')
    const parsed = jsonc.parse(content)
    expect(parsed.mcpServers.other).toEqual({ command: 'npx', args: ['-y', 'other-mcp'] })
    expect(parsed.mcpServers.temps.url).toBe(entry.url)
  })

  test('removeServer is a no-op success when nothing is installed', async () => {
    const result = await adapter.removeServer()
    expect(result).toEqual({ success: true, alreadyInstalled: true })
  })

  test('removeServer deletes the entry and leaves other entries intact', async () => {
    await adapter.addServer(entry)
    const result = await adapter.removeServer()
    expect(result).toEqual({ success: true })
    expect(await adapter.isServerInstalled()).toBe(false)
  })

  test('getServerUrl returns null when not installed, the URL when it is', async () => {
    expect(await adapter.getServerUrl()).toBeNull()
    await adapter.addServer(entry)
    expect(await adapter.getServerUrl()).toBe(entry.url)
  })

  // On Windows, chmod is a no-op for these bits, so this check would be
  // meaningless there -- restrict to platforms where file mode is real.
  const modeMatchers = process.platform === 'win32' ? test.skip : test

  modeMatchers('writes a freshly created config file with mode 0o600', async () => {
    await adapter.addServer(entry)
    const mode = statSync(configPath).mode & 0o777
    expect(mode).toBe(0o600)
  })

  modeMatchers('tightens an existing world-readable config file to 0o600 on overwrite', async () => {
    mkdirSync(join(dir, 'nested'), { recursive: true })
    writeFileSync(configPath, '{ "mcpServers": {} }', { mode: 0o644 })
    chmodSync(configPath, 0o644) // writeFileSync's mode is only honored on create; be explicit.
    expect(statSync(configPath).mode & 0o777).toBe(0o644)

    await adapter.addServer(entry)

    expect(statSync(configPath).mode & 0o777).toBe(0o600)
  })

  modeMatchers('tightens permissions on removeServer as well', async () => {
    await adapter.addServer(entry)
    chmodSync(configPath, 0o644)
    expect(statSync(configPath).mode & 0o777).toBe(0o644)

    await adapter.removeServer()

    expect(statSync(configPath).mode & 0o777).toBe(0o600)
  })
})

describe('the local bridge entry', () => {
  let dir: string
  const entry: McpServerEntry = {
    url: 'https://temps.example.test/mcp?write=1',
    apiKey: 'secret-key',
    bridge: { folders: ['/Users/me/code', '/srv/apps'], packageSpec: '@temps-sdk/cli@1.2.3' },
  }

  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), 'temps-mcp-bridge-'))
  })

  afterEach(() => {
    rmSync(dir, { recursive: true, force: true })
  })

  test('the app starts the pinned CLI with the folders, and the key only in its environment', () => {
    const command = localBridgeCommand(entry, entry.bridge!)
    expect(command).toEqual({
      command: 'npx',
      args: [
        '-y',
        '@temps-sdk/cli@1.2.3',
        'mcp',
        'bridge',
        '--url',
        'https://temps.example.test',
        '--folder',
        '/Users/me/code',
        '--folder',
        '/srv/apps',
      ],
      env: { TEMPS_API_KEY: 'secret-key' },
    })
    expect(command.args.join(' ')).not.toContain('secret-key')
  })

  test('with a key file, the environment names the file and holds no key', () => {
    const command = localBridgeCommand(entry, entry.bridge!, '/home/me/.temps/mcp/codex.key')
    expect(command.env).toEqual({ TEMPS_API_KEY_FILE: '/home/me/.temps/mcp/codex.key' })
    expect(JSON.stringify(command)).not.toContain('secret-key')
  })

  test('the key file is written for this user only, replaced in place, and removed', async () => {
    const file = await writeBridgeKeyFile('codex', 'secret-key', dir)
    expect(file).toBe(bridgeKeyFilePath('codex', dir))
    expect(file).toBe(join(dir, '.temps', 'mcp', 'codex.key'))
    expect(readFileSync(file, 'utf8').trim()).toBe('secret-key')
    expect(statSync(file).mode & 0o777).toBe(0o600)
    expect(statSync(join(dir, '.temps', 'mcp')).mode & 0o777).toBe(0o700)
    // A file a looser earlier version left behind is replaced at 0600.
    chmodSync(file, 0o644)
    await writeBridgeKeyFile('codex', 'second-key', dir)
    expect(readFileSync(file, 'utf8').trim()).toBe('second-key')
    expect(statSync(file).mode & 0o777).toBe(0o600)
    await removeBridgeKeyFile('codex', dir)
    await removeBridgeKeyFile('codex', dir)
    expect(() => statSync(file)).toThrow()
  })

  test('a JSON-config app gets a stdio entry, and status reads it back as read/write', async () => {
    const configPath = join(dir, 'config.json')
    const adapter = new TestAdapter(configPath)
    expect(await adapter.addServer(entry)).toEqual({ success: true })
    const written = JSON.parse(readFileSync(configPath, 'utf8'))
    expect(written.mcpServers.temps).toEqual(localBridgeCommand(entry, entry.bridge!))
    expect(statSync(configPath).mode & 0o777).toBe(0o600)
    expect(await adapter.getServerUrl()).toBe('https://temps.example.test/mcp?write=1')
  })

  test('VS Code marks the entry as stdio', async () => {
    const { VsCodeAdapter } = await import('./vscode.js')
    class TempVsCode extends VsCodeAdapter {
      protected override getConfigPath(): string {
        return join(dir, 'mcp.json')
      }
    }
    await new TempVsCode().addServer(entry)
    const written = JSON.parse(readFileSync(join(dir, 'mcp.json'), 'utf8'))
    expect(written.servers.temps.type).toBe('stdio')
    expect(written.servers.temps.command).toBe('npx')
  })

  test('the URL of a bridge entry, from its arguments or from printed text', () => {
    const args = localBridgeCommand(entry, entry.bridge!).args
    expect(bridgeUrlFromArgs(args)).toBe('https://temps.example.test/mcp?write=1')
    expect(bridgeUrlFromArgs(['-y', 'mcp-remote', 'https://temps.example.test/mcp'])).toBeNull()
    expect(bridgeUrlFromArgs('not a list')).toBeNull()
    expect(
      bridgeUrlFromText(
        'temps:\n  Type: stdio\n  Command: npx\n  Args: -y @temps-sdk/cli@1.2.3 mcp bridge --url https://temps.example.test --folder /srv/apps\n',
      ),
    ).toBe('https://temps.example.test/mcp?write=1')
    expect(bridgeUrlFromText('URL: https://temps.example.test/mcp')).toBeNull()
  })
})
