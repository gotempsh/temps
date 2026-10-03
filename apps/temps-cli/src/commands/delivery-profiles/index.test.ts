// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe, spyOn } from 'bun:test'
import { Command } from 'commander'
import {
  PROFILE_SORT_FIELDS,
  buildCreateDeliveryProfileRequest,
  describeCapability,
  pageFooter,
  parseDeliveryProviderKind,
  parseListPaging,
  parsePositiveInt,
  parseProfileSearch,
  pastLastPageMessage,
  registerDeliveryProfilesCommands,
  validateCreateOptions,
} from './index.js'
import type { DeliveryCapabilityResponse } from '../../api/types.gen.js'

/**
 * Run a CLI command the way `temps` does and report how it ended. The API URL
 * points at a closed local port and `process.exit` throws, so if validation
 * ever moved behind `requireAuth` or a request, the test fails here instead of
 * killing the runner or reaching a real server.
 */
async function runCommand(
  register: (program: Command) => void,
  argv: string[]
): Promise<{ exitCode: number | undefined; stderr: string; fetched: boolean }> {
  const saved = {
    url: process.env.TEMPS_API_URL,
    token: process.env.TEMPS_TOKEN,
    exitCode: process.exitCode,
  }
  process.env.TEMPS_API_URL = 'http://127.0.0.1:9'
  process.env.TEMPS_TOKEN = 'test-token-never-sent'
  // Bun ignores `process.exitCode = undefined`, so reset (and restore) with 0,
  // the same exit status as unset. Otherwise one failing command's 1 leaks into
  // every later exitCode assertion and into the test run's own exit status.
  process.exitCode = 0
  const stderr: string[] = []
  const errSpy = spyOn(console, 'error').mockImplementation(
    (...args: unknown[]) => {
      stderr.push(args.join(' '))
    }
  )
  const logSpy = spyOn(console, 'log').mockImplementation(() => {})
  const exitSpy = spyOn(process, 'exit').mockImplementation(((
    code?: number
  ) => {
    throw new Error(`process.exit(${code}) called: validation ran after auth`)
  }) as typeof process.exit)
  const fetchSpy = spyOn(globalThis, 'fetch')
  try {
    const program = new Command().exitOverride()
    register(program)
    await program.parseAsync(argv, { from: 'user' })
    return {
      exitCode: process.exitCode as number | undefined,
      stderr: stderr.join('\n'),
      fetched: fetchSpy.mock.calls.length > 0,
    }
  } finally {
    errSpy.mockRestore()
    logSpy.mockRestore()
    exitSpy.mockRestore()
    fetchSpy.mockRestore()
    process.exitCode = saved.exitCode ?? 0
    if (saved.url === undefined) delete process.env.TEMPS_API_URL
    else process.env.TEMPS_API_URL = saved.url
    if (saved.token === undefined) delete process.env.TEMPS_TOKEN
    else process.env.TEMPS_TOKEN = saved.token
  }
}

function makeCapability(
  overrides: Partial<DeliveryCapabilityResponse> = {}
): DeliveryCapabilityResponse {
  return {
    configured: false,
    name: 'Bunny CDN',
    provider_kind: 'bunny',
    requirements: ['a Bunny delivery profile'],
    setup_path: '/delivery-profiles',
    supported: true,
    ...overrides,
  }
}

describe('parsePositiveInt', () => {
  test('accepts plain positive integers', () => {
    expect(parsePositiveInt('1')).toBe(1)
    expect(parsePositiveInt(' 42 ')).toBe(42)
  })

  test('rejects zero, negatives, decimals and trailing junk', () => {
    for (const value of ['0', '-1', '1.5', '12abc', '', 'abc', '1e3']) {
      expect(parsePositiveInt(value)).toBeUndefined()
    }
  })
})

describe('parseDeliveryProviderKind', () => {
  test('accepts every provider kind case-insensitively', () => {
    expect(parseDeliveryProviderKind('cloudflare')).toBe('cloudflare')
    expect(parseDeliveryProviderKind('Bunny')).toBe('bunny')
    expect(parseDeliveryProviderKind('DIRECT')).toBe('direct')
  })

  test('rejects unknown kinds', () => {
    expect(parseDeliveryProviderKind('fastly')).toBeUndefined()
  })
})

describe('validateCreateOptions', () => {
  test('a cloudflare profile needs only a name and kind', () => {
    expect(
      validateCreateOptions({ name: ' edge ', kind: 'cloudflare' })
    ).toEqual({ kind: 'cloudflare', name: 'edge' })
  })

  test('requires a name', () => {
    expect(validateCreateOptions({ kind: 'cloudflare' })).toEqual({
      error: '--name is required (1 to 100 characters)',
    })
    expect(validateCreateOptions({ name: '   ', kind: 'cloudflare' })).toEqual({
      error: '--name is required (1 to 100 characters)',
    })
  })

  test('rejects names longer than the server allows', () => {
    const result = validateCreateOptions({
      name: 'x'.repeat(101),
      kind: 'cloudflare',
    })
    expect('error' in result && result.error).toContain(
      'at most 100 characters'
    )
  })

  test('names every valid kind when the kind is wrong', () => {
    expect(validateCreateOptions({ name: 'edge', kind: 'fastly' })).toEqual({
      error: 'Invalid --kind "fastly". Use one of: cloudflare, bunny, direct',
    })
  })

  test('rejects Bunny credentials on a non-Bunny profile', () => {
    const result = validateCreateOptions({
      name: 'edge',
      kind: 'cloudflare',
      apiKey: 'k',
    })
    expect('error' in result && result.error).toContain(
      'only valid with --kind bunny'
    )
  })

  test('bunny requires a positive pull zone id', () => {
    const missing = validateCreateOptions({
      name: 'cdn',
      kind: 'bunny',
      apiKey: 'k',
    })
    expect('error' in missing && missing.error).toContain(
      '--pull-zone-id is required'
    )
    const invalid = validateCreateOptions({
      name: 'cdn',
      kind: 'bunny',
      pullZoneId: '0',
      apiKey: 'k',
    })
    expect('error' in invalid && invalid.error).toContain(
      'must be a positive integer'
    )
  })

  test('bunny rejects both --api-key and --api-key-stdin', () => {
    const result = validateCreateOptions({
      name: 'cdn',
      kind: 'bunny',
      pullZoneId: '7',
      apiKey: 'k',
      apiKeyStdin: true,
    })
    expect('error' in result && result.error).toBe(
      'Use either --api-key or --api-key-stdin, not both'
    )
  })

  test('bunny parses the pull zone id', () => {
    expect(
      validateCreateOptions({
        name: 'cdn',
        kind: 'bunny',
        pullZoneId: '12345',
        apiKeyStdin: true,
      })
    ).toEqual({
      kind: 'bunny',
      name: 'cdn',
      pullZoneId: 12345,
    })
  })
})

describe('buildCreateDeliveryProfileRequest', () => {
  test('sends only name and kind for non-Bunny profiles', () => {
    expect(buildCreateDeliveryProfileRequest('edge', 'cloudflare')).toEqual({
      name: 'edge',
      provider_kind: 'cloudflare',
    })
  })

  test('maps Bunny fields to the API snake_case shape', () => {
    expect(
      buildCreateDeliveryProfileRequest('cdn', 'bunny', 12345, 'secret')
    ).toEqual({
      name: 'cdn',
      provider_kind: 'bunny',
      bunny_pull_zone_id: 12345,
      bunny_api_key: 'secret',
    })
  })
})

describe('describeCapability', () => {
  test('reports ready when supported and configured', () => {
    expect(
      describeCapability(makeCapability({ configured: true, requirements: [] }))
    ).toBe('ready')
  })

  test('names what is missing and where to set it up', () => {
    expect(describeCapability(makeCapability())).toBe(
      'needs a Bunny delivery profile (set up at /delivery-profiles)'
    )
  })

  test('falls back to a generic message without requirements or setup path', () => {
    expect(
      describeCapability(makeCapability({ requirements: [], setup_path: null }))
    ).toBe('not configured')
  })

  test('says so when the instance does not support the provider', () => {
    expect(describeCapability(makeCapability({ supported: false }))).toBe(
      'not supported on this instance'
    )
  })
})

describe('parseListPaging', () => {
  test('defaults to the first 20, newest first, like the API', () => {
    expect(parseListPaging({}, PROFILE_SORT_FIELDS)).toEqual({
      value: {
        page: 1,
        page_size: 20,
        sort_by: 'created_at',
        sort_order: 'desc',
      },
    })
  })

  test('accepts every flag, sort values case-insensitively', () => {
    expect(
      parseListPaging(
        { page: '3', pageSize: '100', sortBy: 'Name', sortOrder: 'ASC' },
        PROFILE_SORT_FIELDS
      )
    ).toEqual({
      value: { page: 3, page_size: 100, sort_by: 'name', sort_order: 'asc' },
    })
  })

  test.each(['0', '-1', '1.5', 'abc'])('rejects --page %p', (page) => {
    expect(parseListPaging({ page }, PROFILE_SORT_FIELDS)).toEqual({
      error: `Invalid --page "${page}". It must be a positive integer`,
    })
  })

  test.each(['0', '101', 'ten'])('rejects --page-size %p', (pageSize) => {
    expect(parseListPaging({ pageSize }, PROFILE_SORT_FIELDS)).toEqual({
      error: `Invalid --page-size "${pageSize}". Use a whole number from 1 to 100`,
    })
  })

  test('names the sort fields this list accepts', () => {
    expect(
      parseListPaging({ sortBy: 'hostname' }, PROFILE_SORT_FIELDS)
    ).toEqual({
      error: 'Invalid --sort-by "hostname". Use one of: created_at, name',
    })
  })

  test('rejects an unknown sort order', () => {
    expect(
      parseListPaging({ sortOrder: 'sideways' }, PROFILE_SORT_FIELDS)
    ).toEqual({
      error: 'Invalid --sort-order "sideways". Use asc or desc',
    })
  })
})

describe('parseProfileSearch', () => {
  test('trims the term; blank or absent means no filter', () => {
    expect(parseProfileSearch(undefined)).toEqual({ value: undefined })
    expect(parseProfileSearch('   ')).toEqual({ value: undefined })
    expect(parseProfileSearch('  Edge ')).toEqual({ value: 'Edge' })
  })

  test('counts characters, not bytes, against the limit', () => {
    expect(parseProfileSearch('é'.repeat(100))).toEqual({
      value: 'é'.repeat(100),
    })
    expect(parseProfileSearch('a'.repeat(101))).toEqual({
      error:
        'Invalid --search: it is 101 characters long; profile names have at most 100',
    })
  })
})

describe('page footer', () => {
  test('counts pages from the total and page size', () => {
    expect(pageFooter({ page: 2, page_size: 20, total: 41 }, 'profile')).toBe(
      'Page 2 of 3 (41 profiles)'
    )
  })

  test('uses the singular for one item', () => {
    expect(pageFooter({ page: 1, page_size: 20, total: 1 }, 'profile')).toBe(
      'Page 1 of 1 (1 profile)'
    )
  })

  test('points a page past the end at the last page', () => {
    expect(
      pastLastPageMessage({ page: 5, page_size: 20, total: 41 }, 'profile')
    ).toBe(
      'Page 5 is past the last page: 41 profiles fit on 3 pages. Use --page 3 or lower'
    )
  })
})

describe('delivery-profiles list validation', () => {
  test.each([
    [['--page', '0'], 'Invalid --page "0"'],
    [['--page-size', '101'], 'Invalid --page-size "101"'],
    [['--sort-by', 'hostname'], 'Invalid --sort-by "hostname"'],
    [['--sort-order', 'sideways'], 'Invalid --sort-order "sideways"'],
  ])('%p exits 1 before any request', async (args, message) => {
    const result = await runCommand(registerDeliveryProfilesCommands, [
      'delivery-profiles',
      'list',
      ...args,
    ])
    expect(result.exitCode).toBe(1)
    expect(result.stderr).toContain(message)
    expect(result.fetched).toBe(false)
  })
})

describe('delivery-profiles create validation', () => {
  test.each([
    [['--name', 'edge', '--kind', 'fastly'], 'Invalid --kind "fastly"'],
    [['--kind', 'cloudflare'], '--name is required'],
    [
      ['--name', 'cdn', '--kind', 'bunny', '--api-key', 'k'],
      '--pull-zone-id is required',
    ],
    [
      ['--name', 'cdn', '--kind', 'bunny', '--pull-zone-id', '7', '--yes'],
      'A Bunny API key is required',
    ],
  ])('%p exits 1 before any request', async (args, message) => {
    const result = await runCommand(registerDeliveryProfilesCommands, [
      'delivery-profiles',
      'create',
      ...args,
    ])
    expect(result.exitCode).toBe(1)
    expect(result.stderr).toContain(message)
    expect(result.fetched).toBe(false)
  })

  test('never echoes the api key in an error', async () => {
    const result = await runCommand(registerDeliveryProfilesCommands, [
      'delivery-profiles',
      'create',
      '--name',
      'edge',
      '--kind',
      'cloudflare',
      '--api-key',
      'super-secret-key',
    ])
    expect(result.exitCode).toBe(1)
    expect(result.stderr).not.toContain('super-secret-key')
  })

  test('remove rejects a non-numeric id before any request', async () => {
    const result = await runCommand(registerDeliveryProfilesCommands, [
      'delivery-profiles',
      'remove',
      '--id',
      'abc',
      '--yes',
    ])
    expect(result.exitCode).toBe(1)
    expect(result.stderr).toContain('Invalid profile ID "abc"')
    expect(result.fetched).toBe(false)
  })
})
