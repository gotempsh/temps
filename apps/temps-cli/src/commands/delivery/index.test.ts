// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe, spyOn } from 'bun:test'
import { Command } from 'commander'
import {
  BINDING_SORT_FIELDS,
  buildDeliverySettingsUpdate,
  parseAdoptRecord,
  parseDefaultProfile,
  parseDnsRecordType,
  parseEnvironmentOverride,
  referencedProfileIds,
  registerDeliveryCommands,
  resolveReferencedProfiles,
  validatePreviewOptions,
} from './index.js'
import { parseListPaging } from '../delivery-profiles/index.js'
import type { DeliveryProfileResponse } from '../../api/types.gen.js'

function makeProfile(
  id: number,
  overrides: Partial<DeliveryProfileResponse> = {}
): DeliveryProfileResponse {
  return {
    id,
    name: `profile-${id}`,
    provider_kind: 'cloudflare',
    bunny_pull_zone_id: null,
    bunny_hostname: null,
    created_at: '2026-09-29T12:00:00Z',
    updated_at: '2026-09-29T12:00:00Z',
    ...overrides,
  }
}

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

describe('parseDnsRecordType', () => {
  test('accepts record types case-insensitively', () => {
    expect(parseDnsRecordType('cname')).toBe('CNAME')
    expect(parseDnsRecordType(' aaaa ')).toBe('AAAA')
  })

  test('rejects unknown types', () => {
    expect(parseDnsRecordType('ALIAS')).toBeUndefined()
  })
})

describe('parseDefaultProfile', () => {
  test('parses a profile id', () => {
    expect(parseDefaultProfile('3')).toEqual({ value: 3 })
  })

  test('"none" clears the default', () => {
    expect(parseDefaultProfile('None')).toEqual({ value: null })
  })

  test('rejects anything else', () => {
    expect(parseDefaultProfile('cloudflare')).toEqual({
      error:
        'Invalid --default-profile "cloudflare". Use a profile ID or "none"',
    })
  })
})

describe('parseEnvironmentOverride', () => {
  test('pins an environment to a profile', () => {
    expect(parseEnvironmentOverride('12=3')).toEqual({
      value: { environment_id: 12, profile_id: 3 },
    })
  })

  test('"inherit" sends a null profile so the environment falls back to the project default', () => {
    expect(parseEnvironmentOverride('12=inherit')).toEqual({
      value: { environment_id: 12, profile_id: null },
    })
  })

  test.each(['12', '=3', 'abc=3', '12=3=4', '12=zero'])(
    'rejects %p',
    (value) => {
      const result = parseEnvironmentOverride(value)
      expect('error' in result).toBe(true)
    }
  )
})

describe('buildDeliverySettingsUpdate', () => {
  test('resends the current default when only overrides change, so the PUT does not clear it', () => {
    expect(
      buildDeliverySettingsUpdate({ default_profile_id: 5 }, undefined, [
        { environment_id: 1, profile_id: 2 },
      ])
    ).toEqual({
      default_profile_id: 5,
      environment_overrides: [{ environment_id: 1, profile_id: 2 }],
    })
  })

  test('an explicit default wins over the current one', () => {
    expect(
      buildDeliverySettingsUpdate({ default_profile_id: 5 }, 9, [])
    ).toEqual({
      default_profile_id: 9,
      environment_overrides: [],
    })
  })

  test('an explicit null clears the default', () => {
    expect(
      buildDeliverySettingsUpdate({ default_profile_id: 5 }, null, [])
    ).toEqual({
      default_profile_id: null,
      environment_overrides: [],
    })
  })

  test('a project with no default stays without one', () => {
    expect(buildDeliverySettingsUpdate({}, undefined, [])).toEqual({
      default_profile_id: null,
      environment_overrides: [],
    })
  })
})

describe('parseAdoptRecord', () => {
  test('splits TYPE:name', () => {
    expect(parseAdoptRecord('cname:www')).toEqual({
      value: { record_type: 'CNAME', name: 'www' },
    })
    expect(parseAdoptRecord('A:@')).toEqual({
      value: { record_type: 'A', name: '@' },
    })
  })

  test.each(['www', ':www', 'CNAME:', 'ALIAS:www'])('rejects %p', (value) => {
    expect('error' in parseAdoptRecord(value)).toBe(true)
  })
})

describe('validatePreviewOptions', () => {
  const full = {
    environmentId: '4',
    hostname: ' app.example.com ',
    zone: 'example.com',
    dnsProvider: '2',
    originTarget: '203.0.113.10',
  }

  test('maps flags to the preview request shape', () => {
    expect(validatePreviewOptions(full)).toEqual({
      value: {
        environment_id: 4,
        dns_provider_id: 2,
        hostname: 'app.example.com',
        zone: 'example.com',
        origin_target: '203.0.113.10',
      },
    })
  })

  test('includes an explicit profile only when given', () => {
    const result = validatePreviewOptions({ ...full, profile: '7' })
    expect('value' in result && result.value.delivery_profile_id).toBe(7)
  })

  test('names every missing flag at once', () => {
    expect(validatePreviewOptions({ hostname: 'app.example.com' })).toEqual({
      error:
        'Missing required option(s): --environment-id, --zone, --dns-provider, --origin-target',
    })
  })

  test('rejects non-numeric ids', () => {
    const result = validatePreviewOptions({
      ...full,
      dnsProvider: 'cloudflare',
    })
    expect('error' in result && result.error).toContain(
      'Invalid --dns-provider "cloudflare"'
    )
  })
})

describe('referencedProfileIds', () => {
  test('collects the default and every override profile once, in order', () => {
    expect(
      referencedProfileIds({
        default_profile_id: 7,
        effective_default_profile: null,
        environment_overrides: [
          { environment_id: 1, profile_id: 9 },
          { environment_id: 2, profile_id: null },
          { environment_id: 3, profile_id: 7 },
          { environment_id: 4, profile_id: 3 },
        ],
      })
    ).toEqual([3, 7, 9])
  })

  test('is empty when nothing refers to a profile', () => {
    expect(
      referencedProfileIds({
        default_profile_id: null,
        effective_default_profile: null,
        environment_overrides: [{ environment_id: 1, profile_id: null }],
      })
    ).toEqual([])
  })
})

describe('resolveReferencedProfiles', () => {
  test('fetches only the profiles the settings response does not carry', async () => {
    const requested: number[] = []
    const profiles = await resolveReferencedProfiles(
      {
        default_profile_id: 7,
        effective_default_profile: makeProfile(7),
        environment_overrides: [
          { environment_id: 1, profile_id: 9 },
          { environment_id: 2, profile_id: 9 },
          { environment_id: 3, profile_id: 7 },
        ],
      },
      async (id) => {
        requested.push(id)
        return makeProfile(id)
      }
    )
    expect(requested).toEqual([9])
    expect(profiles.map((profile) => profile.id)).toEqual([7, 9])
  })

  test('leaves out a profile that no longer exists', async () => {
    const profiles = await resolveReferencedProfiles(
      {
        default_profile_id: null,
        effective_default_profile: null,
        environment_overrides: [
          { environment_id: 1, profile_id: 4 },
          { environment_id: 2, profile_id: 5 },
        ],
      },
      async (id) => (id === 4 ? undefined : makeProfile(id))
    )
    expect(profiles.map((profile) => profile.id)).toEqual([5])
  })

  test('propagates a failed lookup instead of mislabelling', async () => {
    await expect(
      resolveReferencedProfiles(
        {
          default_profile_id: null,
          effective_default_profile: null,
          environment_overrides: [{ environment_id: 1, profile_id: 4 }],
        },
        async () => {
          throw new Error('Delivery profile 4 could not be read')
        }
      )
    ).rejects.toThrow('Delivery profile 4 could not be read')
  })
})

describe('bindings list paging', () => {
  test('accepts the binding sort fields', () => {
    expect(
      parseListPaging(
        { sortBy: 'hostname', sortOrder: 'asc' },
        BINDING_SORT_FIELDS
      )
    ).toEqual({
      value: {
        page: 1,
        page_size: 20,
        sort_by: 'hostname',
        sort_order: 'asc',
      },
    })
  })

  test('rejects a profile-only sort field', () => {
    expect(parseListPaging({ sortBy: 'name' }, BINDING_SORT_FIELDS)).toEqual({
      error:
        'Invalid --sort-by "name". Use one of: created_at, hostname, updated_at',
    })
  })
})

describe('delivery command validation', () => {
  test.each([
    [['settings', 'set', '-p', 'my-app'], 'Nothing to update'],
    [['settings', 'set', '-p', 'my-app', '--env', '12'], 'Invalid --env "12"'],
    [
      ['settings', 'set', '-p', 'my-app', '--default-profile', 'x'],
      'Invalid --default-profile "x"',
    ],
    [
      ['bindings', 'preview', '-p', 'my-app', '--hostname', 'app.example.com'],
      'Missing required option(s)',
    ],
    [
      [
        'bindings',
        'apply',
        '-p',
        'my-app',
        '--preview-id',
        'p',
        '--adopt',
        'www',
      ],
      'Invalid --adopt "www"',
    ],
    [
      ['bindings', 'remove', '-p', 'my-app', '--id', 'x', '--yes'],
      'Invalid binding ID "x"',
    ],
    [
      ['bindings', 'list', '-p', 'my-app', '--sort-by', 'name'],
      'Invalid --sort-by "name"',
    ],
    [
      ['bindings', 'list', '-p', 'my-app', '--page-size', '0'],
      'Invalid --page-size "0"',
    ],
    [
      ['bindings', 'list', '-p', 'my-app', '--page', 'last'],
      'Invalid --page "last"',
    ],
  ])('delivery %p exits 1 before any request', async (args, message) => {
    const result = await runCommand(registerDeliveryCommands, [
      'delivery',
      ...args,
    ])
    expect(result.exitCode).toBe(1)
    expect(result.stderr).toContain(message)
    expect(result.fetched).toBe(false)
  })
})
