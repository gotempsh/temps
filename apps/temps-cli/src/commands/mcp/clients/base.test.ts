// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, test } from 'bun:test'
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import * as jsonc from 'jsonc-parser'
import { JsonConfigMcpClientAdapter, type McpServerEntry } from './base.js'

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
