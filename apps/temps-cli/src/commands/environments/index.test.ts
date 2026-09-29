// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe, spyOn } from 'bun:test'
import { Command } from 'commander'
import {
  registerEnvironmentsCommands,
  resolvePreviewInclusion,
  formatEnvVarValue,
  describeForceHttps,
  formatCpu,
  formatMemory,
  parseResourceUpdate,
  parseReplicaCount,
} from './index.js'
import { MAX_CPU_MILLICORES } from '../../lib/cpu.js'
import type { EnvironmentVariableResponse } from '../../api/types.gen.js'

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
  const saved = { url: process.env.TEMPS_API_URL, token: process.env.TEMPS_TOKEN, exitCode: process.exitCode }
  process.env.TEMPS_API_URL = 'http://127.0.0.1:9'
  process.env.TEMPS_TOKEN = 'test-token-never-sent'
  process.exitCode = undefined
  const stderr: string[] = []
  const errSpy = spyOn(console, 'error').mockImplementation((...args: unknown[]) => {
    stderr.push(args.join(' '))
  })
  const logSpy = spyOn(console, 'log').mockImplementation(() => {})
  const exitSpy = spyOn(process, 'exit').mockImplementation(((code?: number) => {
    throw new Error(`process.exit(${code}) called: validation ran after auth`)
  }) as typeof process.exit)
  const fetchSpy = spyOn(globalThis, 'fetch')
  try {
    const program = new Command().exitOverride()
    register(program)
    await program.parseAsync(argv, { from: 'user' })
    return { exitCode: process.exitCode as number | undefined, stderr: stderr.join('\n'), fetched: fetchSpy.mock.calls.length > 0 }
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

function makeVar(overrides: Partial<EnvironmentVariableResponse> = {}): EnvironmentVariableResponse {
  return {
    id: 1,
    key: 'DATABASE_URL',
    value: 'postgres://x',
    is_secret: false,
    include_in_preview: true,
    environments: [],
    ...overrides,
  } as EnvironmentVariableResponse
}

describe('preview variable scope', () => {
  test('the CLI leaves previews excluded until --preview is explicitly supplied', () => {
    for (const [args, expected] of [
      [[], undefined],
      [['--preview'], true],
      [['--no-preview'], false],
    ] as const) {
      const program = new Command()
      registerEnvironmentsCommands(program)
      const set = program.commands.find(command => command.name() === 'environments')!
        .commands.find(command => command.name() === 'vars')!
        .commands.find(command => command.name() === 'set')!
      set.parseOptions([...args])
      expect(set.opts().preview).toBe(expected)
    }
    expect(resolvePreviewInclusion(undefined, false, undefined)).toBe(false)
  })

  test('an update retains the current preview scope unless the operator changes it', () => {
    expect(resolvePreviewInclusion(undefined, true, true)).toBe(true)
    expect(resolvePreviewInclusion(false, true, true)).toBe(false)
    expect(resolvePreviewInclusion(true, true, false)).toBe(true)
  })
})

describe('formatEnvVarValue', () => {
  test('never prints a secret value, even if one somehow arrived on the wire', () => {
    // The API never returns secret plaintext, but the formatter itself must
    // not be the thing that would leak it if that contract ever broke.
    const v = makeVar({ is_secret: true, value: 'super-secret' })
    const out = formatEnvVarValue(v)
    expect(out).not.toContain('super-secret')
    expect(out).toContain('write-only')
  })

  test('shows the plain value for a non-secret variable', () => {
    expect(formatEnvVarValue(makeVar({ value: 'hello' }))).toBe('hello')
  })

  test('renders a missing value as empty rather than "undefined"', () => {
    expect(formatEnvVarValue(makeVar({ value: undefined }))).toBe('')
  })
})

describe('describeForceHttps', () => {
  test('true reads as an explicit always-redirect', () => {
    expect(describeForceHttps(true)).toContain('always redirect')
  })

  test('false reads as an explicit never-redirect', () => {
    expect(describeForceHttps(false)).toContain('never redirect')
  })

  test('null and undefined both read as "inherit", not "disabled"', () => {
    // A missing override must never look the same as an explicit --disable.
    expect(describeForceHttps(null)).toContain('inherit')
    expect(describeForceHttps(undefined)).toContain('inherit')
  })
})

describe('formatCpu', () => {
  test('renders the stored microcores as millicores with the equivalent core count', () => {
    // The API stores CPU in microcores (1_000_000 = one core); the default
    // request of half a core is stored as 500_000.
    expect(formatCpu(500_000)).toBe('500m (0.5 CPU)')
    expect(formatCpu(1_000_000)).toBe('1000m (1 CPU)')
    expect(formatCpu(2_000_000)).toBe('2000m (2 CPU)')
  })

  test('renders an unset limit distinctly from 0', () => {
    expect(formatCpu(null)).toContain('not set')
    expect(formatCpu(undefined)).toContain('not set')
  })
})

describe('formatMemory', () => {
  test('renders sub-GB values in plain MB', () => {
    expect(formatMemory(512)).toBe('512MB')
  })

  test('adds a GB conversion once memory crosses 1024MB', () => {
    expect(formatMemory(2048)).toBe('2048MB (2.0GB)')
  })

  test('renders an unset limit distinctly from 0', () => {
    expect(formatMemory(null)).toContain('not set')
  })
})

describe('parseResourceUpdate', () => {
  test('rejects a non-numeric or non-positive CPU value', () => {
    for (const cpu of ['abc', '0', '-5', '1000abc', '1.5']) {
      const result = parseResourceUpdate({ cpu })
      expect('error' in result && result.error).toContain('CPU must be a positive whole number of millicores')
    }
  })

  test('rejects a CPU limit below Docker minimum of 0.01 cores', () => {
    expect(parseResourceUpdate({ cpu: '9' })).toEqual({
      error: 'CPU must be at least 10 millicores (0.01 cores), got 9',
    })
    expect(parseResourceUpdate({ cpu: '10' })).toEqual({
      body: { cpu_limit: 10_000, cpu_request: 10_000 },
    })
  })

  test('rejects memory with trailing junk or decimals', () => {
    for (const memory of ['512mb', '1.5']) {
      expect(parseResourceUpdate({ memory })).toEqual({ error: 'Memory must be a positive number (MB)' })
    }
  })

  test('rejects a non-numeric or non-positive memory value', () => {
    expect(parseResourceUpdate({ memory: '-5' })).toEqual({
      error: 'Memory must be a positive number (MB)',
    })
  })

  test('converts CPU millicores to the microcores the API stores', () => {
    // 1000 millicores is one core, which the API stores as 1_000_000.
    // Sending the millicore value unconverted would cap the container at
    // 0.001 cores.
    expect(parseResourceUpdate({ cpu: '1000' })).toEqual({
      body: { cpu_limit: 1_000_000, cpu_request: 1_000_000 },
    })
    expect(parseResourceUpdate({ cpu: '500', cpuRequest: '250' })).toEqual({
      body: { cpu_limit: 500_000, cpu_request: 250_000 },
    })
  })

  test('rejects implausibly large CPU values', () => {
    expect(parseResourceUpdate({ cpu: String(MAX_CPU_MILLICORES) })).toEqual({
      body: {
        cpu_limit: MAX_CPU_MILLICORES * 1000,
        cpu_request: MAX_CPU_MILLICORES * 1000,
      },
    })
    const tooBig = parseResourceUpdate({ cpu: String(MAX_CPU_MILLICORES + 1) })
    expect('error' in tooBig && tooBig.error).toContain(`CPU must be at most ${MAX_CPU_MILLICORES} millicores`)
    const requestTooBig = parseResourceUpdate({ cpu: '1000', cpuRequest: String(MAX_CPU_MILLICORES + 1) })
    expect('error' in requestTooBig && requestTooBig.error).toContain(
      `CPU request must be at most ${MAX_CPU_MILLICORES} millicores`
    )
  })

  test('points users of the old microcore workaround at the new unit', () => {
    // CLI 0.1.36 and earlier sent --cpu unconverted, so the docs told users to
    // pass microcores. Those values must not silently become 1000 cores.
    const result = parseResourceUpdate({ cpu: '1000000', cpuRequest: '500000' })
    expect('error' in result && result.error).toContain('divide by 1000')
  })

  test('defaults the request to the limit when no explicit request is given', () => {
    // Otherwise a container gets a limit with no guaranteed minimum, which
    // the scheduler treats as "no request" rather than "same as limit".
    const result = parseResourceUpdate({ cpu: '1000', memory: '512' })
    expect(result).toEqual({
      body: { cpu_limit: 1_000_000, cpu_request: 1_000_000, memory_limit: 512, memory_request: 512 },
    })
  })

  test('an explicit request overrides the limit-derived default', () => {
    const result = parseResourceUpdate({ cpu: '1000', cpuRequest: '250' })
    expect(result).toEqual({ body: { cpu_limit: 1_000_000, cpu_request: 250_000 } })
  })

  test('rejects a non-positive explicit request even when the limit is valid', () => {
    const result = parseResourceUpdate({ cpu: '1000', cpuRequest: '0' })
    expect('error' in result && result.error).toContain('CPU request must be a positive whole number of millicores')
  })

  test('leaves fields untouched when nothing is set', () => {
    expect(parseResourceUpdate({})).toEqual({ body: {} })
  })
})

describe('invalid resource flags', () => {
  // A script using the old `--cpu 1000000` microcore workaround must stop, not
  // print an error and carry on as if the limit had been applied.
  test.each([
    [['--cpu', '1000000', '--cpu-request', '500000'], 'divide by 1000'],
    [['--cpu', '5'], 'at least 10 millicores'],
    [['--cpu', '1000abc'], 'positive whole number of millicores'],
    [['--memory', '512mb'], 'Memory must be a positive number'],
  ])('environments resources %p exits 1 before any request', async (flags, message) => {
    const result = await runCommand(registerEnvironmentsCommands, [
      'environments', 'resources', 'production', '-p', 'my-app', ...flags,
    ])
    expect(result.exitCode).toBe(1)
    expect(result.stderr).toContain(message)
    expect(result.fetched).toBe(false)
  })

  test('environments scale with an invalid replica count exits 1 before any request', async () => {
    const result = await runCommand(registerEnvironmentsCommands, [
      'environments', 'scale', '-p', 'my-app', '--replicas', 'many',
    ])
    expect(result.exitCode).toBe(1)
    expect(result.stderr).toContain('Replicas must be a non-negative number')
    expect(result.fetched).toBe(false)
  })
})

describe('parseReplicaCount', () => {
  test('accepts zero (scale to nothing)', () => {
    expect(parseReplicaCount('0')).toEqual({ replicas: 0 })
  })

  test('rejects a negative count', () => {
    expect(parseReplicaCount('-1')).toEqual({ error: 'Replicas must be a non-negative number' })
  })

  test('rejects non-numeric input', () => {
    expect(parseReplicaCount('many')).toEqual({ error: 'Replicas must be a non-negative number' })
  })

  test('warns, but still succeeds, above 10 replicas', () => {
    const result = parseReplicaCount('25')
    expect(result).toMatchObject({ replicas: 25 })
    expect('warning' in result && result.warning).toContain('25 replicas')
  })

  test('does not warn at or below 10 replicas', () => {
    expect(parseReplicaCount('10')).toEqual({ replicas: 10 })
  })
})
