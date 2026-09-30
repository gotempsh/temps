// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe, spyOn } from 'bun:test'
import { Command } from 'commander'
import {
  buildDnsRecordContent,
  describeRecordContent,
  parseRecordKey,
} from './records.js'
import { registerDnsCommands } from './index.js'

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
  process.exitCode = undefined
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
    process.exitCode = saved.exitCode
    if (saved.url === undefined) delete process.env.TEMPS_API_URL
    else process.env.TEMPS_API_URL = saved.url
    if (saved.token === undefined) delete process.env.TEMPS_TOKEN
    else process.env.TEMPS_TOKEN = saved.token
  }
}

describe('buildDnsRecordContent', () => {
  test('A and AAAA carry the value as an address', () => {
    expect(buildDnsRecordContent('A', { value: '203.0.113.10' })).toEqual({
      value: { type: 'A', value: { address: '203.0.113.10' } },
    })
    expect(buildDnsRecordContent('AAAA', { value: '2001:db8::1' })).toEqual({
      value: { type: 'AAAA', value: { address: '2001:db8::1' } },
    })
  })

  test('CNAME and PTR carry the value as a target', () => {
    expect(
      buildDnsRecordContent('CNAME', { value: 'origin.example.net' })
    ).toEqual({
      value: { type: 'CNAME', value: { target: 'origin.example.net' } },
    })
    expect(buildDnsRecordContent('PTR', { value: 'host.example.com' })).toEqual(
      {
        value: { type: 'PTR', value: { target: 'host.example.com' } },
      }
    )
  })

  test('TXT and NS use their own field names', () => {
    expect(buildDnsRecordContent('TXT', { value: 'v=spf1 -all' })).toEqual({
      value: { type: 'TXT', value: { content: 'v=spf1 -all' } },
    })
    expect(buildDnsRecordContent('NS', { value: 'ns1.example.com' })).toEqual({
      value: { type: 'NS', value: { nameserver: 'ns1.example.com' } },
    })
  })

  test('MX requires a priority', () => {
    expect(
      buildDnsRecordContent('MX', { value: 'mail.example.com', priority: '10' })
    ).toEqual({
      value: {
        type: 'MX',
        value: { priority: 10, target: 'mail.example.com' },
      },
    })
    expect(buildDnsRecordContent('MX', { value: 'mail.example.com' })).toEqual({
      error: '--priority is required for MX records',
    })
  })

  test('SRV requires priority, weight and port', () => {
    expect(
      buildDnsRecordContent('SRV', {
        value: 'sip.example.com',
        priority: '10',
        weight: '5',
        port: '5060',
      })
    ).toEqual({
      value: {
        type: 'SRV',
        value: {
          priority: 10,
          weight: 5,
          port: 5060,
          target: 'sip.example.com',
        },
      },
    })
    expect(
      buildDnsRecordContent('SRV', {
        value: 'sip.example.com',
        priority: '10',
        weight: '5',
      })
    ).toEqual({
      error: '--port is required for SRV records',
    })
  })

  test('rejects out-of-range numeric fields', () => {
    const result = buildDnsRecordContent('SRV', {
      value: 't',
      priority: '1',
      weight: '1',
      port: '70000',
    })
    expect('error' in result && result.error).toContain('from 0 to 65535')
  })

  test('CAA requires a tag and defaults flags to 0', () => {
    expect(
      buildDnsRecordContent('CAA', { value: 'letsencrypt.org', tag: 'issue' })
    ).toEqual({
      value: {
        type: 'CAA',
        value: { flags: 0, tag: 'issue', value: 'letsencrypt.org' },
      },
    })
    expect(buildDnsRecordContent('CAA', { value: 'letsencrypt.org' })).toEqual({
      error: '--tag is required for CAA records (issue, issuewild or iodef)',
    })
    const tooBig = buildDnsRecordContent('CAA', {
      value: 'letsencrypt.org',
      tag: 'issue',
      flags: '256',
    })
    expect('error' in tooBig && tooBig.error).toContain('from 0 to 255')
  })

  test('every type requires --value', () => {
    expect(buildDnsRecordContent('A', {})).toEqual({
      error: '--value is required for A records',
    })
    expect(buildDnsRecordContent('TXT', { value: '  ' })).toEqual({
      error: '--value is required for TXT records',
    })
  })
})

describe('describeRecordContent', () => {
  test('renders each shape as a single line', () => {
    expect(
      describeRecordContent({ type: 'A', value: { address: '203.0.113.10' } })
    ).toBe('203.0.113.10')
    expect(
      describeRecordContent({
        type: 'MX',
        value: { priority: 10, target: 'mail.example.com' },
      })
    ).toBe('10 mail.example.com')
    expect(
      describeRecordContent({
        type: 'CAA',
        value: { flags: 0, tag: 'issue', value: 'letsencrypt.org' },
      })
    ).toBe('0 issue "letsencrypt.org"')
  })
})

describe('parseRecordKey', () => {
  test('normalizes the record type', () => {
    expect(
      parseRecordKey({ domain: 'example.com', name: 'www', type: 'cname' })
    ).toEqual({
      value: { domain: 'example.com', name: 'www', record_type: 'CNAME' },
    })
  })

  test('lists valid types when the type is wrong', () => {
    const result = parseRecordKey({
      domain: 'example.com',
      name: 'www',
      type: 'ALIAS',
    })
    expect('error' in result && result.error).toBe(
      'Invalid --type "ALIAS". Use one of: A, AAAA, CNAME, TXT, MX, NS, SRV, CAA, PTR'
    )
  })

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

describe('dns records validation', () => {
  const key = ['--domain', 'example.com', '--name', 'www']
  test.each([
    [
      ['set', ...key, '--type', 'ALIAS', '--value', 'x'],
      'Invalid --type "ALIAS"',
    ],
    [['set', ...key, '--type', 'A'], '--value is required for A records'],
    [
      ['set', ...key, '--type', 'A', '--value', '203.0.113.10', '--ttl', '-5'],
      'Invalid --ttl "-5"',
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
    [['import', ...key, '--type', 'ALIAS'], 'Invalid --type "ALIAS"'],
    [['ownership', ...key, '--type', 'ALIAS'], 'Invalid --type "ALIAS"'],
    [['remove', ...key, '--type', 'ALIAS', '--yes'], 'Invalid --type "ALIAS"'],
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
