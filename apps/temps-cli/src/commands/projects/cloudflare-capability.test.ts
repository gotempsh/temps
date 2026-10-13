// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe, spyOn } from 'bun:test'
import { Command } from 'commander'
import {
  defaultDeliveryProvider,
  parseDeliveryProviderChoice,
} from './cloudflare-capability.js'
import { registerProjectsCommands } from './index.js'
import type { CloudflareProjectCapability } from '../../api/types.gen.js'

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
  overrides: Partial<CloudflareProjectCapability> = {}
): CloudflareProjectCapability {
  return {
    configured: false,
    default_enabled: false,
    bunny_configured: false,
    bunny_default_enabled: false,
    ...overrides,
  }
}

describe('parseDeliveryProviderChoice', () => {
  test('accepts the values the API accepts, case-insensitively', () => {
    expect(parseDeliveryProviderChoice('none')).toBe('none')
    expect(parseDeliveryProviderChoice('Cloudflare')).toBe('cloudflare')
    expect(parseDeliveryProviderChoice(' bunny ')).toBe('bunny')
  })

  test('rejects anything else, including the internal "direct" kind', () => {
    expect(parseDeliveryProviderChoice('direct')).toBeUndefined()
    expect(parseDeliveryProviderChoice('fastly')).toBeUndefined()
  })
})

describe('defaultDeliveryProvider', () => {
  test('Cloudflare wins when both defaults are on, matching the server', () => {
    expect(
      defaultDeliveryProvider(
        makeCapability({ default_enabled: true, bunny_default_enabled: true })
      )
    ).toBe('cloudflare')
  })

  test('falls back to Bunny, then none', () => {
    expect(
      defaultDeliveryProvider(makeCapability({ bunny_default_enabled: true }))
    ).toBe('bunny')
    expect(defaultDeliveryProvider(makeCapability())).toBe('none')
  })
})

describe('projects create --delivery-provider', () => {
  test('an unknown provider exits 1 before any request', async () => {
    const result = await runCommand(registerProjectsCommands, [
      'projects',
      'create',
      '--name',
      'my-app',
      '--manual',
      '--yes',
      '--delivery-provider',
      'fastly',
    ])
    expect(result.exitCode).toBe(1)
    expect(result.stderr).toContain(
      'Invalid --delivery-provider "fastly". Use one of: none, cloudflare, bunny'
    )
    expect(result.fetched).toBe(false)
  })
})
