// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe, spyOn } from 'bun:test'
import { Command } from 'commander'
import {
  MANAGED_DNS_RECORD_TYPES,
  buildDnsRecordContent,
  describeRecordContent,
  parseRecordKey,
  parseTtl,
  type ManagedDnsRecordType,
} from './records.js'
import { registerDnsCommands } from './index.js'

/** Types the API can describe but every managed-record endpoint rejects. */
const UNSUPPORTED_TYPES = ['TXT', 'MX', 'NS', 'SRV', 'CAA', 'PTR']

function ttlError(value: string): string {
  return `Invalid --ttl "${value}". Use 60 to 86400 seconds, or 1 for the provider default`
}

function unsupportedTypeError(type: string): string {
  return `Unsupported --type "${type}". Managed DNS records support A, AAAA and CNAME; manage other record types directly at your DNS provider`
}

/** A `dns records` subcommand, registered the way `temps` registers it. */
function recordsSubcommand(name: string): Command {
  const program = new Command()
  registerDnsCommands(program)
  const command = program.commands
    .find((c) => c.name() === 'dns')
    ?.commands.find((c) => c.name() === 'records')
    ?.commands.find((c) => c.name() === name)
  if (!command) throw new Error(`dns records ${name} is not registered`)
  return command
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

describe('MANAGED_DNS_RECORD_TYPES', () => {
  test('is limited to the routing types the server manages', () => {
    expect(MANAGED_DNS_RECORD_TYPES).toEqual(['A', 'AAAA', 'CNAME'])
  })
})

describe('buildDnsRecordContent', () => {
  test('A and AAAA carry the value as an address', () => {
    expect(buildDnsRecordContent('A', { value: '203.0.113.10' })).toEqual({
      value: { type: 'A', value: { address: '203.0.113.10' } },
    })
    expect(buildDnsRecordContent('AAAA', { value: '2001:db8::1' })).toEqual({
      value: { type: 'AAAA', value: { address: '2001:db8::1' } },
    })
  })

  test('CNAME carries the value as a target', () => {
    expect(
      buildDnsRecordContent('CNAME', { value: 'origin.example.net' })
    ).toEqual({
      value: { type: 'CNAME', value: { target: 'origin.example.net' } },
    })
  })

  test('every type requires --value', () => {
    expect(buildDnsRecordContent('A', {})).toEqual({
      error: '--value is required for A records',
    })
    expect(buildDnsRecordContent('CNAME', { value: '  ' })).toEqual({
      error: '--value is required for CNAME records',
    })
  })
})

describe('describeRecordContent', () => {
  test('renders routing records as their address or target', () => {
    expect(
      describeRecordContent({ type: 'A', value: { address: '203.0.113.10' } })
    ).toBe('203.0.113.10')
    expect(
      describeRecordContent({ type: 'AAAA', value: { address: '2001:db8::1' } })
    ).toBe('2001:db8::1')
    expect(
      describeRecordContent({
        type: 'CNAME',
        value: { target: 'origin.example.net' },
      })
    ).toBe('origin.example.net')
  })

  test('shows any other content as raw JSON instead of dropping it', () => {
    expect(
      describeRecordContent({
        type: 'MX',
        value: { priority: 10, target: 'mail.example.com' },
      })
    ).toBe('{"priority":10,"target":"mail.example.com"}')
  })
})

describe('parseRecordKey', () => {
  const accepted: [string, ManagedDnsRecordType][] = [
    ['A', 'A'],
    ['aaaa', 'AAAA'],
    [' cname ', 'CNAME'],
  ]
  test.each(accepted)('accepts %p as %p', (type, recordType) => {
    expect(parseRecordKey({ domain: 'example.com', name: 'www', type })).toEqual(
      {
        value: { domain: 'example.com', name: 'www', record_type: recordType },
      }
    )
  })

  test.each([...UNSUPPORTED_TYPES, 'mx', 'ALIAS'])(
    'rejects %p and names the supported types',
    (type) => {
      const result = parseRecordKey({ domain: 'example.com', name: 'www', type })
      expect('error' in result && result.error).toBe(unsupportedTypeError(type))
    }
  )

  test('rejects an empty name', () => {
    const result = parseRecordKey({
      domain: 'example.com',
      name: ' ',
      type: 'A',
    })
    expect('error' in result && result.error).toContain(
      'use "@" for the zone apex'
    )
  })
})

describe('parseTtl', () => {
  test('leaves the TTL unset when --ttl is omitted', () => {
    expect(parseTtl(undefined)).toEqual({ value: undefined })
  })

  const accepted: [string, number][] = [
    ['1', 1],
    ['60', 60],
    [' 300 ', 300],
    ['86400', 86_400],
  ]
  test.each(accepted)('accepts %p as %p', (value, ttl) => {
    expect(parseTtl(value)).toEqual({ value: ttl })
  })

  test.each(['0', '2', '59', '86401', '-5', '1.5', '300s', 'abc'])(
    'rejects %p and names the allowed range',
    (value) => {
      expect(parseTtl(value)).toEqual({ error: ttlError(value) })
    }
  )
})

describe('dns records help', () => {
  test.each(['ownership', 'set', 'import', 'remove'])(
    '%s offers only A, AAAA and CNAME',
    (name) => {
      const command = recordsSubcommand(name)
      const typeOption = command.options.find(
        (option) => option.long === '--type'
      )
      expect(typeOption?.description).toBe('Record type (A, AAAA, CNAME)')
      const help = command.helpInformation()
      for (const type of UNSUPPORTED_TYPES) {
        expect(help).not.toMatch(new RegExp(`\\b${type}\\b`))
      }
    }
  )

  test('set has no MX, SRV or CAA content flags', () => {
    const flags = recordsSubcommand('set').options.map((option) => option.long)
    expect(flags).toContain('--value')
    for (const flag of ['--priority', '--weight', '--port', '--flags', '--tag']) {
      expect(flags).not.toContain(flag)
    }
  })

  test('set names the TTL range the server accepts', () => {
    const ttl = recordsSubcommand('set').options.find(
      (option) => option.long === '--ttl'
    )
    expect(ttl?.description).toBe(
      'TTL in seconds, 60-86400; omit (or use 1) for the provider default'
    )
  })
})

describe('dns records validation', () => {
  const key = ['--domain', 'example.com', '--name', 'www']
  test.each([
    [
      ['set', ...key, '--type', 'ALIAS', '--value', 'x'],
      unsupportedTypeError('ALIAS'),
    ],
    [
      ['set', ...key, '--type', 'TXT', '--value', 'v=spf1 -all'],
      unsupportedTypeError('TXT'),
    ],
    [
      ['set', ...key, '--type', 'mx', '--value', 'mail.example.com'],
      unsupportedTypeError('mx'),
    ],
    [['set', ...key, '--type', 'A'], '--value is required for A records'],
    [
      ['set', ...key, '--type', 'A', '--value', '203.0.113.10', '--ttl', '-5'],
      ttlError('-5'),
    ],
    [
      ['set', ...key, '--type', 'A', '--value', '203.0.113.10', '--ttl', '30'],
      ttlError('30'),
    ],
    [
      [
        'set',
        ...key,
        '--type',
        'CNAME',
        '--value',
        'origin.example.net',
        '--ttl',
        '86401',
      ],
      ttlError('86401'),
    ],
    [
      [
        'set',
        ...key,
        '--type',
        'A',
        '--value',
        '203.0.113.10',
        '--environment-id',
        'prod',
      ],
      'Invalid --environment-id "prod"',
    ],
    [['import', ...key, '--type', 'TXT'], unsupportedTypeError('TXT')],
    [['ownership', ...key, '--type', 'NS'], unsupportedTypeError('NS')],
    [
      ['remove', ...key, '--type', 'CAA', '--yes'],
      unsupportedTypeError('CAA'),
    ],
  ])('dns records %p exits 1 before any request', async (args, message) => {
    const result = await runCommand(registerDnsCommands, [
      'dns',
      'records',
      ...args,
    ])
    expect(result.exitCode).toBe(1)
    expect(result.stderr).toContain(message)
    expect(result.fetched).toBe(false)
  })
})
