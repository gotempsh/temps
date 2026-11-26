// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test, spyOn } from 'bun:test'
import { codexTokenExportLine, formatMcpAccess, originOf, registerMcpCommands } from './index.js'

// originOf is the comparison primitive that decides whether a credential
// bound to one URL may be reused for another (see ensureMcpAuth /
// findContextByUrl). It must be strict: two URLs are "the same target" only
// when their scheme, host, and port all match exactly -- never a substring,
// prefix, or host-only match -- otherwise a mistyped, malicious, or
// scheme-downgraded URL could be treated as matching a saved, trusted
// context and inherit its credential.
describe('originOf', () => {
  test('extracts scheme+host+port from a URL', () => {
    expect(originOf('http://localhost:3000/mcp')).toBe('http://localhost:3000')
  })

  test('extracts scheme+host when no port is given (default port implied)', () => {
    expect(originOf('https://temps.example.com/mcp')).toBe('https://temps.example.com')
  })

  test('is insensitive to path and query string', () => {
    expect(originOf('https://temps.example.com/mcp?groups=platform&write=1')).toBe('https://temps.example.com')
    expect(originOf('https://temps.example.com')).toBe('https://temps.example.com')
  })

  test('different ports on the same hostname do not match', () => {
    expect(originOf('http://localhost:3000')).not.toBe(originOf('http://localhost:8080'))
  })

  test('a subdomain does not match its parent domain', () => {
    expect(originOf('https://evil.temps.example.com')).not.toBe(originOf('https://temps.example.com'))
  })

  test('a lookalike host is not treated as the same as the real one', () => {
    expect(originOf('https://temps.example.com.evil.net')).not.toBe(originOf('https://temps.example.com'))
  })

  test('http and https on the same host do NOT match (scheme downgrade must not inherit a credential)', () => {
    expect(originOf('http://temps.example.com')).not.toBe(originOf('https://temps.example.com'))
  })

  test('returns null for an unparsable URL rather than throwing', () => {
    expect(originOf('not a url')).toBeNull()
  })
})

describe('mcp add wizard', () => {
  test.each([false, true])('connects without selecting tool groups (write=%s)', async (write) => {
    const { Command } = await import('commander')
    const contexts = await import('../../config/contexts.js')
    const api = await import('../../lib/api-client.js')
    const prompts = await import('../../ui/prompts.js')
    const probe = await import('./probe.js')
    const { getClientAdapter } = await import('./clients/index.js')
    const adapter = getClientAdapter('claude-code')!
    const confirmations: string[] = []
    const spies = [
      spyOn(contexts, 'listContexts').mockResolvedValue([
        { name: 'test', url: 'https://temps.example.test', apiKey: 'test-key', email: 'test@example.test' },
      ]),
      spyOn(api, 'setupClient').mockResolvedValue(undefined),
      spyOn(probe, 'probeMcpEndpoint').mockResolvedValue({ supported: true }),
      spyOn(adapter, 'isClientSupported').mockResolvedValue(true),
      spyOn(adapter, 'describeTarget').mockResolvedValue('test configuration'),
      spyOn(console, 'log').mockImplementation(() => {}),
      spyOn(prompts, 'promptConfirm').mockImplementation(async (options) => {
        confirmations.push(options.message)
        return options.message.startsWith('Enable temps_write') ? write : true
      }),
    ]
    const checkbox = spyOn(prompts, 'promptCheckbox').mockImplementation(async () => {
      throw new Error('The virtual CLI must not ask for tool groups')
    })
    const add = spyOn(adapter, 'addServer').mockResolvedValue({ success: true })
    try {
      const program = new Command().exitOverride()
      registerMcpCommands(program)
      await program.parseAsync(
        ['mcp', 'add', 'claude-code', '--url', 'https://temps.example.test', '--api-key', 'test-key'],
        { from: 'user' },
      )
      expect(checkbox).not.toHaveBeenCalled()
      expect(add).toHaveBeenCalledWith({
        url: `https://temps.example.test/mcp${write ? '?write=1' : ''}`,
        apiKey: 'test-key',
      })
      expect(confirmations).toEqual([
        'Enable temps_write? Writes are proposed first and require confirm_action before execution.',
        'Apply this configuration?',
      ])
    } finally {
      add.mockRestore()
      checkbox.mockRestore()
      spies.forEach((spy) => spy.mockRestore())
    }
  })
})

describe('mcp add -y', () => {
  test.each([
    [false, 'reader'],
    [true, null],
  ] as const)('read-only installs a new reader key, never the login (write=%s)', async (write, minted) => {
    const { Command } = await import('commander')
    const contexts = await import('../../config/contexts.js')
    const api = await import('../../lib/api-client.js')
    const sdk = await import('../../api/sdk.gen.js')
    const probe = await import('./probe.js')
    const { getClientAdapter } = await import('./clients/index.js')
    const adapter = getClientAdapter('cursor')!
    const created: unknown[] = []
    const spies = [
      spyOn(contexts, 'listContexts').mockResolvedValue([
        { name: 'test', url: 'https://temps.example.test', apiKey: 'login-key', email: 'test@example.test' },
      ]),
      spyOn(api, 'setupClient').mockResolvedValue(undefined),
      spyOn(probe, 'probeMcpEndpoint').mockResolvedValue({ supported: true }),
      spyOn(adapter, 'isClientSupported').mockResolvedValue(true),
      spyOn(adapter, 'describeTarget').mockResolvedValue('test configuration'),
      spyOn(console, 'log').mockImplementation(() => {}),
      spyOn(sdk, 'createApiKey').mockImplementation((async (options: { body: unknown }) => {
        created.push(options.body)
        return { data: { api_key: 'tk_reader', name: 'mcp-cursor', key_prefix: 'tk_r' }, error: undefined }
      }) as never),
    ]
    const add = spyOn(adapter, 'addServer').mockResolvedValue({ success: true })
    try {
      const program = new Command().exitOverride()
      registerMcpCommands(program)
      await program.parseAsync(
        ['mcp', 'add', 'cursor', '--url', 'https://temps.example.test', ...(write ? ['--write'] : []), '-y'],
        { from: 'user' },
      )
      const installed = (add.mock.calls[0]?.[0] as { apiKey: string }).apiKey
      if (minted) {
        expect(created).toHaveLength(1)
        expect((created[0] as { role_type: string }).role_type).toBe(minted)
        expect(installed).toBe('tk_reader')
      } else {
        expect(created).toEqual([])
        expect(installed).toBe('login-key')
      }
    } finally {
      add.mockRestore()
      spies.forEach((spy) => spy.mockRestore())
    }
  })
})

describe('mcp add --deploy-folder', () => {
  test('the app runs the bridge with the folders, pinned to this CLI, with write tools and no write prompt', async () => {
    const { Command } = await import('commander')
    const { mkdtempSync, rmSync, realpathSync } = await import('node:fs')
    const { tmpdir } = await import('node:os')
    const { join } = await import('node:path')
    const api = await import('../../lib/api-client.js')
    const prompts = await import('../../ui/prompts.js')
    const probe = await import('./probe.js')
    const contexts = await import('../../config/contexts.js')
    const { getClientAdapter } = await import('./clients/index.js')
    const adapter = getClientAdapter('cursor')!
    const folder = realpathSync(mkdtempSync(join(tmpdir(), 'temps-deploy-folder-')))
    const confirmations: string[] = []
    const spies = [
      spyOn(contexts, 'listContexts').mockResolvedValue([
        { name: 'test', url: 'https://temps.example.test', apiKey: 'test-key', email: 'test@example.test' },
      ]),
      spyOn(api, 'setupClient').mockResolvedValue(undefined),
      spyOn(probe, 'probeMcpEndpoint').mockResolvedValue({ supported: true }),
      spyOn(adapter, 'isClientSupported').mockResolvedValue(true),
      spyOn(adapter, 'describeTarget').mockResolvedValue('test configuration'),
      spyOn(console, 'log').mockImplementation(() => {}),
      spyOn(prompts, 'promptConfirm').mockImplementation(async (options) => {
        confirmations.push(options.message)
        return true
      }),
    ]
    const add = spyOn(adapter, 'addServer').mockResolvedValue({ success: true })
    try {
      const program = new Command().exitOverride().version('9.8.7')
      registerMcpCommands(program)
      await program.parseAsync(
        [
          'mcp',
          'add',
          'cursor',
          '--url',
          'https://temps.example.test',
          '--api-key',
          'test-key',
          '--deploy-folder',
          folder,
        ],
        { from: 'user' },
      )
      expect(add).toHaveBeenCalledWith({
        url: 'https://temps.example.test/mcp?write=1',
        apiKey: 'test-key',
        bridge: { folders: [folder], packageSpec: '@temps-sdk/cli@9.8.7' },
      })
      expect(confirmations).toEqual(['Apply this configuration?'])
    } finally {
      add.mockRestore()
      spies.forEach((spy) => spy.mockRestore())
      rmSync(folder, { recursive: true, force: true })
    }
  })

  test('a folder that does not exist, or an address the bridge refuses, writes nothing', async () => {
    const { Command } = await import('commander')
    const api = await import('../../lib/api-client.js')
    const probe = await import('./probe.js')
    const contexts = await import('../../config/contexts.js')
    const output = await import('../../ui/output.js')
    const { getClientAdapter } = await import('./clients/index.js')
    const adapter = getClientAdapter('cursor')!
    const warnings: string[] = []
    const spies = [
      spyOn(contexts, 'listContexts').mockResolvedValue([
        { name: 'test', url: 'https://temps.example.test', apiKey: 'test-key', email: 'test@example.test' },
        { name: 'lan', url: 'http://192.168.1.10:8080', apiKey: 'test-key', email: 'test@example.test' },
      ]),
      spyOn(api, 'setupClient').mockResolvedValue(undefined),
      spyOn(probe, 'probeMcpEndpoint').mockResolvedValue({ supported: true }),
      spyOn(adapter, 'isClientSupported').mockResolvedValue(true),
      spyOn(console, 'log').mockImplementation(() => {}),
      spyOn(output, 'warning').mockImplementation((message: string) => {
        warnings.push(message)
      }),
    ]
    const add = spyOn(adapter, 'addServer').mockResolvedValue({ success: true })
    try {
      for (const [url, folder] of [
        ['https://temps.example.test', '/definitely/not/a/folder'],
        ['http://192.168.1.10:8080', process.cwd()],
      ] as const) {
        const program = new Command().exitOverride()
        registerMcpCommands(program)
        await program.parseAsync(
          ['mcp', 'add', 'cursor', '--url', url, '--api-key', 'test-key', '--deploy-folder', folder, '-y'],
          { from: 'user' },
        )
      }
      expect(add).not.toHaveBeenCalled()
      expect(warnings[0]).toContain('is not a folder on this computer')
      expect(warnings[1]).toContain('plain HTTP')
    } finally {
      add.mockRestore()
      spies.forEach((spy) => spy.mockRestore())
    }
  })
})

test('MCP status describes read/write access and labels legacy group filters', () => {
  expect(formatMcpAccess('https://temps.example.test/mcp')).toBe('Read-only')
  expect(formatMcpAccess('https://temps.example.test/mcp?write=1')).toBe('Read/write (confirmation)')
  expect(formatMcpAccess('https://temps.example.test/mcp?groups=platform')).toBe('Read-only; legacy groups: platform')
  expect(formatMcpAccess(null)).toBe('-')
  expect(formatMcpAccess('invalid')).toBe('-')
})

test('the Codex token export shows only a key this command minted', () => {
  const key = 's3cr3t-test-value'
  expect(codexTokenExportLine(key, true)).toContain(`Bearer ${key}`)
  const supplied = codexTokenExportLine(key, false)
  expect(supplied).not.toContain(key)
  expect(supplied).toContain('<your Temps API key>')
})
