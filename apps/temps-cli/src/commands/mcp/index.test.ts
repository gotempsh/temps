// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, test, spyOn } from 'bun:test'
import { codexTokenExportLine, formatMcpAccess, mintedKeyName, originOf, registerMcpCommands } from './index.js'
import * as ledger from './minted-keys.js'
import { keyDigest, type MintedKey } from './minted-keys.js'

// No test reads or writes the real ledger in the user's home directory.
let ledgerSpies: {
  mintedKeyFor: ReturnType<typeof spyOn<typeof ledger, 'mintedKeyFor'>>
  mintedKeysFor: ReturnType<typeof spyOn<typeof ledger, 'mintedKeysFor'>>
  recordedDigests: ReturnType<typeof spyOn<typeof ledger, 'recordedDigests'>>
  recordMintedKey: ReturnType<typeof spyOn<typeof ledger, 'recordMintedKey'>>
  forgetMintedKey: ReturnType<typeof spyOn<typeof ledger, 'forgetMintedKey'>>
}
beforeEach(() => {
  ledgerSpies = {
    mintedKeyFor: spyOn(ledger, 'mintedKeyFor').mockResolvedValue(undefined),
    mintedKeysFor: spyOn(ledger, 'mintedKeysFor').mockResolvedValue([]),
    recordedDigests: spyOn(ledger, 'recordedDigests').mockResolvedValue(new Set()),
    recordMintedKey: spyOn(ledger, 'recordMintedKey').mockResolvedValue(undefined),
    forgetMintedKey: spyOn(ledger, 'forgetMintedKey').mockResolvedValue(undefined),
  }
})
afterEach(() => {
  ledgerSpies.mintedKeyFor.mockRestore()
  ledgerSpies.mintedKeysFor.mockRestore()
  ledgerSpies.recordedDigests.mockRestore()
  ledgerSpies.recordMintedKey.mockRestore()
  ledgerSpies.forgetMintedKey.mockRestore()
})

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
        {
          name: 'test',
          url: 'https://temps.example.test',
          apiKey: 'test-key',
          email: 'test@example.test',
        },
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
    const add = spyOn(adapter, 'addServer').mockResolvedValue({
      success: true,
    })
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
    [true, 'user'],
  ] as const)('installs a new key named for the app, never the login (write=%s)', async (write, minted) => {
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
        {
          name: 'test',
          url: 'https://temps.example.test',
          apiKey: 'login-key',
          email: 'test@example.test',
        },
      ]),
      spyOn(api, 'setupClient').mockResolvedValue(undefined),
      spyOn(probe, 'probeMcpEndpoint').mockResolvedValue({ supported: true }),
      spyOn(adapter, 'isClientSupported').mockResolvedValue(true),
      spyOn(adapter, 'describeTarget').mockResolvedValue('test configuration'),
      spyOn(console, 'log').mockImplementation(() => {}),
      spyOn(sdk, 'createApiKey').mockImplementation((async (options: { body: unknown }) => {
        created.push(options.body)
        return {
          data: {
            api_key: 'tk_reader',
            name: 'mcp-cursor',
            key_prefix: 'tk_r',
          },
          error: undefined,
        }
      }) as never),
    ]
    const add = spyOn(adapter, 'addServer').mockResolvedValue({
      success: true,
    })
    try {
      const program = new Command().exitOverride()
      registerMcpCommands(program)
      await program.parseAsync(
        ['mcp', 'add', 'cursor', '--url', 'https://temps.example.test', ...(write ? ['--write'] : []), '-y'],
        { from: 'user' },
      )
      const installed = (add.mock.calls[0]?.[0] as { apiKey: string }).apiKey
      expect(created).toHaveLength(1)
      expect((created[0] as { role_type: string }).role_type).toBe(minted)
      // Named for the app, so the console's welcome step sees it connect.
      expect((created[0] as { name: string }).name).toMatch(/^mcp-cursor-/)
      expect(installed).toBe('tk_reader')
    } finally {
      add.mockRestore()
      spies.forEach((spy) => spy.mockRestore())
    }
  })
})

describe('the key an install replaces or leaves behind', () => {
  const ORIGIN = 'https://temps.example.test'
  const OLD = {
    origin: ORIGIN,
    id: 41,
    name: 'mcp-cursor-2026-11-20-bbbbbb',
    sha256: keyDigest('tk_old_secret'),
  }

  interface Run {
    /** What `mcp add`/`remove` recorded for this install before. */
    recorded?: MintedKey
    /** The key the app's entry sends before the command. */
    before?: string
    /** The key it sends once this install wrote its entry (by default the one written). */
    after?: string
    result?: { success: boolean; alreadyInstalled?: boolean }
    /** What Temps answers for the recorded key's ID. */
    stored?: { status: number; name?: string; key_prefix?: string }
    contexts?: { name: string; url: string; apiKey: string; email: string }[]
    /** Other keys recorded for this install before the command, besides `recorded`. */
    others?: MintedKey[]
    /** Keys another install running at the same time records once this one started. */
    racing?: MintedKey[]
    /** The entry cannot be read back once written, as Codex's HTTP entry, whose key is in the shell. */
    unreadable?: boolean
    /** The ledger cannot be listed when the command starts. */
    unlistable?: boolean
    /** The key the entry sends once this install settled its record: another install's, written meanwhile. */
    afterSettle?: string
    /** What Temps answers for other IDs than the recorded key's. */
    storedById?: Record<number, { status: number; name?: string; key_prefix?: string }>
    /** Logged in only through the environment (TEMPS_API_KEY), with no --url. */
    ambient?: boolean
  }

  /** Run `mcp <argv>` for `client`; returns the key IDs deleted, what was recorded and what was printed. */
  async function run(argv: string[], options: Run, client = 'cursor') {
    const { Command } = await import('commander')
    const contexts = await import('../../config/contexts.js')
    const api = await import('../../lib/api-client.js')
    const sdk = await import('../../api/sdk.gen.js')
    const probe = await import('./probe.js')
    const { getClientAdapter } = await import('./clients/index.js')
    const adapter = getClientAdapter(client)!
    const deleted: number[] = []
    const printed: string[] = []
    const recordedAfter: (MintedKey | null)[] = []
    const clients: unknown[] = []
    // A record is found by the key the entry sends, as on disk.
    ledgerSpies.mintedKeyFor.mockImplementation(async (_scope: string, installed: string | undefined) =>
      installed && options.recorded?.sha256 === keyDigest(installed) ? options.recorded : undefined,
    )
    const atStart = [...(options.recorded ? [options.recorded] : []), ...(options.others ?? [])]
    ledgerSpies.recordedDigests.mockImplementation(async () => {
      if (options.unlistable) throw new Error('EACCES: permission denied, scandir')
      return new Set(atStart.map((key) => key.sha256))
    })
    ledgerSpies.mintedKeysFor.mockImplementation(async () => [...atStart, ...(options.racing ?? [])])
    // A record settled is written down apart, by its key's digest.
    const settled: string[] = []
    ledgerSpies.recordMintedKey.mockImplementation(async (_scope: string, key: MintedKey) => {
      if (key.settled) {
        settled.push(key.sha256)
        if (options.afterSettle) sends = options.afterSettle
      } else recordedAfter.push(key)
    })
    // A record forgotten is written down as null.
    ledgerSpies.forgetMintedKey.mockImplementation(async () => {
      recordedAfter.push(null)
    })
    let sends = options.before
    const outcome = options.result ?? { success: true }
    const stored = options.stored ?? {
      status: 200,
      name: OLD.name,
      key_prefix: 'tk_old',
    }
    const store = await import('../../config/store.js')
    const ambient = options.ambient
      ? [
          spyOn(store.credentials, 'isAuthenticated').mockResolvedValue(true),
          spyOn(store, 'requireAuth').mockResolvedValue('env-key' as never),
          spyOn(api, 'getWebUrl').mockReturnValue(ORIGIN),
        ]
      : []
    const spies = [
      ...ambient,
      spyOn(contexts, 'listContexts').mockResolvedValue(
        options.contexts ?? [
          {
            name: 'test',
            url: ORIGIN,
            apiKey: 'login-key',
            email: 'test@example.test',
          },
        ],
      ),
      spyOn(api, 'setupClient').mockImplementation((async (...args: unknown[]) => {
        clients.push(args)
      }) as never),
      spyOn(probe, 'probeMcpEndpoint').mockResolvedValue({ supported: true }),
      spyOn(adapter, 'isClientSupported').mockResolvedValue(true),
      spyOn(adapter, 'describeTarget').mockResolvedValue('test configuration'),
      spyOn(adapter, 'installedApiKey').mockImplementation(async () => sends),
      spyOn(adapter, 'addServer').mockImplementation(async (entry: { apiKey: string }) => {
        if (outcome.success && !outcome.alreadyInstalled) {
          sends = options.unreadable ? undefined : (options.after ?? entry.apiKey)
        }
        return outcome as never
      }),
      spyOn(adapter, 'removeServer').mockResolvedValue(options.result ?? { success: true }),
      spyOn(console, 'log').mockImplementation((...args: unknown[]) => {
        printed.push(args.join(' '))
      }),
      spyOn(console, 'warn').mockImplementation((...args: unknown[]) => {
        printed.push(args.join(' '))
      }),
      spyOn(sdk, 'createApiKey').mockImplementation((async (options: { body: { name: string } }) => ({
        data: {
          id: 50,
          api_key: 'tk_new_secret',
          name: options.body.name,
          key_prefix: 'tk_new',
        },
        error: undefined,
      })) as never),
      spyOn(sdk, 'getApiKey').mockImplementation((async ({ path }: { path: { id: number } }) => {
        const answer = options.storedById?.[path.id] ?? stored
        return answer.status === 200
          ? {
              data: { id: path.id, ...answer },
              error: undefined,
              response: { status: 200 },
            }
          : {
              data: undefined,
              error: { detail: 'not found' },
              response: { status: answer.status },
            }
      }) as never),
      spyOn(sdk, 'deleteApiKey').mockImplementation((async (options: { path: { id: number } }) => {
        deleted.push(options.path.id)
        return { data: undefined, error: undefined }
      }) as never),
    ]
    const previousExitCode = process.exitCode
    try {
      process.exitCode = 0
      const program = new Command().exitOverride()
      registerMcpCommands(program)
      await program.parseAsync(['mcp', ...argv], { from: 'user' })
      return {
        deleted,
        recordedAfter,
        settled,
        clients,
        printed: printed.join('\n'),
        exitCode: process.exitCode,
      }
    } finally {
      process.exitCode = previousExitCode ?? 0
      spies.forEach((spy) => spy.mockRestore())
    }
  }
  const add = (options: Run) =>
    run(options.ambient ? ['add', 'cursor', '-y'] : ['add', 'cursor', '--url', ORIGIN, '-y'], options)
  // The key another install, run at the same time, minted and recorded.
  const RACED: MintedKey = {
    origin: ORIGIN,
    id: 42,
    name: 'mcp-cursor-2026-11-20-cccccc',
    sha256: keyDigest('tk_raced_secret'),
    settled: true,
  }
  const racedStored = { 42: { status: 200, name: RACED.name, key_prefix: 'tk_raced' } }
  const remove = (options: Run) => run(['remove', 'cursor'], options)

  test('the key mcp add minted for this install, still in its entry, is deleted and the new one recorded', async () => {
    const { deleted, recordedAfter } = await add({
      recorded: OLD,
      before: 'tk_old_secret',
    })
    expect(deleted).toEqual([41])
    // The new key is recorded, then the old one forgotten.
    expect(recordedAfter).toHaveLength(2)
    expect(recordedAfter[0]).toMatchObject({
      origin: ORIGIN,
      id: 50,
      sha256: keyDigest('tk_new_secret'),
    })
    expect(recordedAfter[0]?.name).toMatch(/^mcp-cursor-/)
    expect(recordedAfter[1]).toBeNull()
  })

  test('an install run at the same time whose entry this one replaced: its recorded key is deleted', async () => {
    const { deleted, recordedAfter, settled } = await add({ racing: [RACED], storedById: racedStored })
    expect(deleted).toEqual([42])
    expect(recordedAfter[0]).toMatchObject({ id: 50, sha256: keyDigest('tk_new_secret') })
    expect(recordedAfter.slice(1)).toEqual([null])
    // Its own record is settled, for an install that replaces it later.
    expect(settled).toEqual([keyDigest('tk_new_secret')])
  })

  test('the key of an install still writing its entry, or one recorded before this command, is kept', async () => {
    // Not settled yet: that install finds out itself whether it lost.
    const writing = await add({ racing: [{ ...RACED, settled: false }], storedById: racedStored })
    expect(writing.deleted).toEqual([])
    // Recorded before: its entry was replaced before this command, by the
    // user perhaps, who may use the key elsewhere.
    const before = await add({ others: [RACED], storedById: racedStored })
    expect(before.deleted).toEqual([])
  })

  test("an entry it cannot read back, or a ledger it could not list, deletes no other install's key", async () => {
    // Codex keeps an HTTP entry's key in the shell: never settled, so no
    // bridge install later takes the exported key for one it replaced.
    const unread = await add({ racing: [RACED], storedById: racedStored, unreadable: true })
    expect(unread.deleted).toEqual([])
    expect(unread.settled).toEqual([])
    // An older record could not be told from one another install made now.
    const unlisted = await add({ racing: [RACED], storedById: racedStored, unlistable: true })
    expect(unlisted.deleted).toEqual([])
    expect(unlisted.settled).toEqual([keyDigest('tk_new_secret')])
  })

  test('the install whose entry was replaced deletes only its own key, never the one sent now', async () => {
    const lost = await add({ racing: [RACED], storedById: racedStored, after: 'tk_raced_secret' })
    expect(lost.deleted).toEqual([50])
    expect(lost.printed).toContain('Another temps mcp add changed')
    expect(lost.settled).toEqual([])
  })

  test('an entry replaced while this install settled its record: its own key is deleted all the same', async () => {
    const lost = await add({ afterSettle: 'tk_raced_secret' })
    expect(lost.deleted).toEqual([50])
    expect(lost.recordedAfter.at(-1)).toBeNull()
    expect(lost.printed).toContain('Another temps mcp add changed')
  })

  test('logged in only through the environment, a key on that instance is deleted with that login', async () => {
    const { deleted, clients } = await add({
      ambient: true,
      recorded: OLD,
      before: 'tk_old_secret',
      contexts: [],
    })
    expect(deleted).toEqual([41])
    // No saved login was looked for or set up for it.
    expect(clients).toEqual([[undefined, 'env-key']])
  })

  test('an entry that sends another key keeps it: the user put it there', async () => {
    const { deleted } = await add({ recorded: OLD, before: 'tk_users_own' })
    expect(deleted).toEqual([])
  })

  test('nothing recorded for this install (another profile, or a copied config): nothing is deleted', async () => {
    expect((await add({ before: 'tk_old_secret' })).deleted).toEqual([])
    expect((await add({})).deleted).toEqual([])
  })

  test('a key recorded on another instance with no login saved for it is kept, and the user told', async () => {
    const elsewhere = { ...OLD, origin: 'https://other.example.test' }
    const { deleted, printed } = await add({
      recorded: elsewhere,
      before: 'tk_old_secret',
    })
    expect(deleted).toEqual([])
    expect(printed).toContain('https://other.example.test is unused now')
  })

  test('moving an app to another instance deletes its key on the first, with the login saved there', async () => {
    const first = { ...OLD, origin: 'https://other.example.test' }
    const { deleted, clients } = await add({
      recorded: first,
      before: 'tk_old_secret',
      contexts: [
        {
          name: 'test',
          url: ORIGIN,
          apiKey: 'login-key',
          email: 'test@example.test',
        },
        {
          name: 'other',
          url: 'https://other.example.test',
          apiKey: 'other-login',
          email: 'test@example.test',
        },
      ],
    })
    expect(deleted).toEqual([41])
    // Deleted with the first instance's login, then back to this one's.
    expect(clients.slice(-2)).toEqual([
      ['https://other.example.test', 'other-login'],
      [ORIGIN, 'login-key'],
    ])
  })

  test('another install into the same settings at once: the one whose entry stays keeps its key, the other deletes its own', async () => {
    const { deleted, recordedAfter, printed } = await add({
      recorded: OLD,
      before: 'tk_old_secret',
      after: 'tk_from_the_other_install',
    })
    // Only the key minted here goes; the old one is the winner's to retire.
    expect(deleted).toEqual([50])
    expect(recordedAfter).toHaveLength(2)
    expect(recordedAfter[0]).toMatchObject({ id: 50 })
    expect(recordedAfter[1]).toBeNull()
    expect(printed).toContain('at the same time')
  })

  test('a key under that ID that is not the recorded one is kept, and one already gone is no error', async () => {
    const renamed = { status: 200, name: 'ci deploys', key_prefix: 'tk_old' }
    expect((await add({ recorded: OLD, before: 'tk_old_secret', stored: renamed })).deleted).toEqual([])
    const otherPrefix = { status: 200, name: OLD.name, key_prefix: 'tk_xyz' }
    expect(
      (
        await add({
          recorded: OLD,
          before: 'tk_old_secret',
          stored: otherPrefix,
        })
      ).deleted,
    ).toEqual([])
    expect(
      (
        await add({
          recorded: OLD,
          before: 'tk_old_secret',
          stored: { status: 404 },
        })
      ).deleted,
    ).toEqual([])
  })

  test('an install already in place deletes only the key just minted, and records nothing', async () => {
    const { deleted, recordedAfter } = await add({
      recorded: OLD,
      before: 'tk_old_secret',
      result: { success: true, alreadyInstalled: true },
    })
    expect(deleted).toEqual([50])
    expect(recordedAfter).toEqual([])
  })

  test('a failed install deletes only the key just minted, and records nothing', async () => {
    const { deleted, recordedAfter } = await add({
      recorded: OLD,
      before: 'tk_old_secret',
      result: { success: false },
    })
    expect(deleted).toEqual([50])
    expect(recordedAfter).toEqual([])
  })

  test('a new entry that sends the recorded key itself keeps it, and it stays recorded', async () => {
    const { deleted, recordedAfter } = await run(
      ['add', 'cursor', '--url', ORIGIN, '--api-key', 'tk_old_secret', '--groups', 'projects', '-y'],
      { recorded: OLD, before: 'tk_old_secret' },
    )
    expect(deleted).toEqual([])
    expect(recordedAfter).toEqual([])
  })

  test('the recorded key sent to another instance is kept there and forgotten here, so removal never deletes it', async () => {
    const { deleted, recordedAfter } = await run(
      ['add', 'cursor', '--url', 'https://other.example.test', '--api-key', 'tk_old_secret', '-y'],
      {
        recorded: OLD,
        before: 'tk_old_secret',
        contexts: [
          {
            name: 'test',
            url: ORIGIN,
            apiKey: 'login-key',
            email: 'test@example.test',
          },
          {
            name: 'other',
            url: 'https://other.example.test',
            apiKey: 'other-login',
            email: 'test@example.test',
          },
        ],
      },
    )
    expect(deleted).toEqual([])
    expect(recordedAfter).toEqual([null])
  })

  test('an app that refuses the entry fails the command, as does one that will not remove it', async () => {
    const { deleted, exitCode } = await add({ result: { success: false } })
    expect(deleted).toEqual([50])
    expect(exitCode).toBe(1)
    expect((await remove({ result: { success: false } })).exitCode).toBe(1)
  })

  test('Codex already set up: the key just minted and deleted is never shown to export', async () => {
    const { deleted, printed } = await run(
      ['add', 'codex', '--url', ORIGIN, '-y'],
      {
        result: { success: true, alreadyInstalled: true },
      },
      'codex',
    )
    expect(deleted).toEqual([50])
    expect(printed).not.toContain('tk_new_secret')
    expect(printed).toContain('<your Temps API key>')
  })

  test('removing an entry deletes the key minted for it, with the login saved for its instance', async () => {
    const { deleted, recordedAfter, clients } = await remove({
      recorded: OLD,
      before: 'tk_old_secret',
    })
    expect(deleted).toEqual([41])
    expect(clients).toEqual([[ORIGIN, 'login-key']])
    expect(recordedAfter).toEqual([null])
  })

  test('removing an entry without a login for its instance deletes nothing and forgets it', async () => {
    const { deleted, recordedAfter } = await remove({
      recorded: OLD,
      before: 'tk_old_secret',
      contexts: [],
    })
    expect(deleted).toEqual([])
    expect(recordedAfter).toEqual([null])
  })

  test('removing an entry that sends a key mcp add did not record here deletes nothing', async () => {
    expect((await remove({ recorded: OLD, before: 'tk_users_own' })).deleted).toEqual([])
    expect((await remove({ before: 'tk_old_secret' })).deleted).toEqual([])
    const nothing = await remove({
      recorded: OLD,
      before: 'tk_old_secret',
      result: { success: true, alreadyInstalled: true },
    })
    expect(nothing).toMatchObject({ deleted: [], recordedAfter: [] })
  })
})

describe('the key mcp add creates', () => {
  test('two installs on the same day get different names, so a retry is not refused', () => {
    const day = new Date('2026-11-26T10:00:00Z')
    const first = mintedKeyName('cursor', day)
    expect(first).toMatch(/^mcp-cursor-2026-11-26-[0-9a-f]{6}$/)
    expect(mintedKeyName('cursor', day)).not.toBe(first)
  })

  test.each([[{ success: false, reason: 'the app refused it' }], [{ success: true, alreadyInstalled: true }]] as const)(
    'is deleted again when the app is not configured with it (%o)',
    async (outcome) => {
      const { Command } = await import('commander')
      const contexts = await import('../../config/contexts.js')
      const api = await import('../../lib/api-client.js')
      const sdk = await import('../../api/sdk.gen.js')
      const probe = await import('./probe.js')
      const { getClientAdapter } = await import('./clients/index.js')
      const adapter = getClientAdapter('cursor')!
      const deleted: unknown[] = []
      const spies = [
        spyOn(contexts, 'listContexts').mockResolvedValue([
          {
            name: 'test',
            url: 'https://temps.example.test',
            apiKey: 'login-key',
            email: 'test@example.test',
          },
        ]),
        spyOn(api, 'setupClient').mockResolvedValue(undefined),
        spyOn(probe, 'probeMcpEndpoint').mockResolvedValue({ supported: true }),
        spyOn(adapter, 'isClientSupported').mockResolvedValue(true),
        spyOn(adapter, 'describeTarget').mockResolvedValue('test configuration'),
        spyOn(console, 'log').mockImplementation(() => {}),
        spyOn(sdk, 'createApiKey').mockImplementation((async () => ({
          data: {
            id: 41,
            api_key: 'tk_reader',
            name: 'mcp-cursor',
            key_prefix: 'tk_r',
          },
          error: undefined,
        })) as never),
        spyOn(sdk, 'deleteApiKey').mockImplementation((async (options: { path: unknown }) => {
          deleted.push(options.path)
          return { data: undefined, error: undefined }
        }) as never),
      ]
      const add = spyOn(adapter, 'addServer').mockResolvedValue(outcome)
      // A refusal sets the exit code; the test runner's own is restored.
      const previousExitCode = process.exitCode
      try {
        const program = new Command().exitOverride()
        registerMcpCommands(program)
        await program.parseAsync(['mcp', 'add', 'cursor', '--url', 'https://temps.example.test', '-y'], {
          from: 'user',
        })
        expect(add).toHaveBeenCalledTimes(1)
        expect(deleted).toEqual([{ id: 41 }])
        expect(process.exitCode).toBe(outcome.success ? previousExitCode : 1)
      } finally {
        process.exitCode = previousExitCode ?? 0
        add.mockRestore()
        spies.forEach((spy) => spy.mockRestore())
      }
    },
  )
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
        {
          name: 'test',
          url: 'https://temps.example.test',
          apiKey: 'test-key',
          email: 'test@example.test',
        },
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
    const add = spyOn(adapter, 'addServer').mockResolvedValue({
      success: true,
    })
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
        {
          name: 'test',
          url: 'https://temps.example.test',
          apiKey: 'test-key',
          email: 'test@example.test',
        },
        {
          name: 'lan',
          url: 'http://192.168.1.10:8080',
          apiKey: 'test-key',
          email: 'test@example.test',
        },
      ]),
      spyOn(api, 'setupClient').mockResolvedValue(undefined),
      spyOn(probe, 'probeMcpEndpoint').mockResolvedValue({ supported: true }),
      spyOn(adapter, 'isClientSupported').mockResolvedValue(true),
      spyOn(console, 'log').mockImplementation(() => {}),
      spyOn(output, 'warning').mockImplementation((message: string) => {
        warnings.push(message)
      }),
    ]
    const add = spyOn(adapter, 'addServer').mockResolvedValue({
      success: true,
    })
    try {
      for (const [url, folder, ...extra] of [
        ['https://temps.example.test', '/definitely/not/a/folder'],
        ['http://192.168.1.10:8080', process.cwd()],
        ['https://temps.example.test', '  '],
        ['https://temps.example.test', process.cwd(), '--groups', 'platform'],
      ] as const) {
        const program = new Command().exitOverride()
        registerMcpCommands(program)
        await program.parseAsync(
          ['mcp', 'add', 'cursor', '--url', url, '--api-key', 'test-key', '--deploy-folder', folder, '-y', ...extra],
          { from: 'user' },
        )
      }
      expect(add).not.toHaveBeenCalled()
      expect(warnings[0]).toContain('is not a folder on this computer')
      expect(warnings[1]).toContain('plain HTTP')
      expect(warnings[2]).toContain('left blank')
      expect(warnings[3]).toContain('--groups cannot be combined with --deploy-folder')
      // Nothing was installed, so the command fails for a script.
      expect(process.exitCode).toBe(1)
    } finally {
      process.exitCode = 0
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
