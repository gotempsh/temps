// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe, spyOn } from 'bun:test'
import { Command } from 'commander'
import { parseCpuLimitCores } from './update.js'
import { registerProjectsCommands } from './index.js'

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

describe('parseCpuLimitCores', () => {
  test('converts cores to the microcores the API stores', () => {
    // The API stores CPU in microcores (1_000_000 = one core). Sending the
    // core count unconverted would store `2` as two microcores.
    expect(parseCpuLimitCores('2')).toEqual({ microcores: 2_000_000 })
    expect(parseCpuLimitCores('1')).toEqual({ microcores: 1_000_000 })
    expect(parseCpuLimitCores('0.5')).toEqual({ microcores: 500_000 })
    expect(parseCpuLimitCores('0.25')).toEqual({ microcores: 250_000 })
  })

  test('rounds to a whole number of microcores', () => {
    expect(parseCpuLimitCores('0.1')).toEqual({ microcores: 100_000 })
    expect(parseCpuLimitCores('1.0000004')).toEqual({ microcores: 1_000_000 })
  })

  test('rejects non-numeric, zero, negative and trailing-junk values', () => {
    for (const value of ['abc', '0', '-1', '1abc', '1e3', 'Infinity']) {
      const result = parseCpuLimitCores(value)
      expect('error' in result && result.error).toContain('positive number of cores')
    }
  })

  test('rejects limits below Docker minimum of 0.01 cores', () => {
    expect(parseCpuLimitCores('0.01')).toEqual({ microcores: 10_000 })
    for (const value of ['0.005', '0.0000001']) {
      const result = parseCpuLimitCores(value)
      expect('error' in result && result.error).toContain('at least 0.01 cores')
    }
  })

  test('rejects implausibly large values', () => {
    expect(parseCpuLimitCores('256')).toEqual({ microcores: 256_000_000 })
    const result = parseCpuLimitCores('257')
    expect('error' in result && result.error).toContain('at most 256 cores')
  })
})

describe('projects config with an invalid --cpu-limit', () => {
  test.each([
    ['0.005', 'at least 0.01 cores'],
    ['257', 'at most 256 cores'],
    ['1abc', 'positive number of cores'],
  ])('--cpu-limit %p exits 1 before any request', async (value, message) => {
    const result = await runCommand(registerProjectsCommands, [
      'projects', 'config', '-p', 'my-app', '--cpu-limit', value,
    ])
    expect(result.exitCode).toBe(1)
    expect(result.stderr).toContain(message)
    expect(result.fetched).toBe(false)
  })
})
