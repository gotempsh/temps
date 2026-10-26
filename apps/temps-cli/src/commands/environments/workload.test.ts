// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, spyOn, test } from 'bun:test'
import { Command } from 'commander'
import { registerEnvironmentsCommands } from './index.js'
import {
  buildCreateVolumeRequest,
  describeAttachment,
  parseVolumeRef,
  parseWindowDays,
  parseWorkloadIsolation,
  resolveRequireLogin,
  validateMountPath,
  validVolumeName,
} from './workload.js'
import type { EnvironmentVolumeResponse } from '../../api/types.gen.js'

/**
 * Run a command with the API pointed at a closed port and `process.exit`
 * trapped, reporting whether anything was fetched: validation must fail
 * before authentication or any request.
 */
async function run(argv: string[]): Promise<{ exitCode: number | undefined; stderr: string; fetched: boolean }> {
  const saved = { url: process.env.TEMPS_API_URL, token: process.env.TEMPS_TOKEN, exitCode: process.exitCode }
  process.env.TEMPS_API_URL = 'http://127.0.0.1:9'
  process.env.TEMPS_TOKEN = 'test-token-never-sent'
  process.exitCode = 0
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
    registerEnvironmentsCommands(program)
    await program.parseAsync(argv, { from: 'user' })
    return { exitCode: process.exitCode as number | undefined, stderr: stderr.join('\n'), fetched: fetchSpy.mock.calls.length > 0 }
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

function subcommand(path: string[]): Command {
  const program = new Command()
  registerEnvironmentsCommands(program)
  let current = program.commands.find((c) => c.name() === 'environments')!
  for (const name of path) {
    const next = current.commands.find((c) => c.name() === name)
    if (!next) throw new Error(`missing subcommand ${path.join(' ')}`)
    current = next
  }
  return current
}

function flags(command: Command): string[] {
  return command.options.map((o) => o.long ?? '')
}

describe('registration', () => {
  test('every placement, isolation, volume and ingress subcommand is registered', () => {
    for (const path of [
      ['placement', 'explain'],
      ['placement', 'recommend'],
      ['isolation', 'show'],
      ['isolation', 'set'],
      ['volumes', 'list'],
      ['volumes', 'create'],
      ['volumes', 'status'],
      ['volumes', 'attach'],
      ['volumes', 'detach'],
      ['volumes', 'rm'],
      ['ingress', 'show'],
      ['ingress', 'set'],
    ]) {
      expect(flags(subcommand(path))).toContain('--project')
    }
    expect(flags(subcommand(['placement', 'recommend']))).toContain('--window-days')
    expect(flags(subcommand(['volumes', 'create']))).toEqual(
      expect.arrayContaining(['--name', '--size-mb', '--retain'])
    )
    expect(flags(subcommand(['volumes', 'attach']))).toEqual(
      expect.arrayContaining(['--mount-path', '--read-only'])
    )
    expect(flags(subcommand(['volumes', 'rm']))).toEqual(expect.arrayContaining(['--force', '--yes']))
  })

  test('ingress set offers only the login requirement, never the ingress mode', () => {
    const set = flags(subcommand(['ingress', 'set']))
    expect(set).toEqual(expect.arrayContaining(['--require-login', '--no-require-login']))
    expect(set.some((flag) => flag.includes('mode'))).toBe(false)
  })

  test('the legacy/shadow/enforce workload mode switch has no command', () => {
    const program = new Command()
    registerEnvironmentsCommands(program)
    const environments = program.commands.find((c) => c.name() === 'environments')!
    expect(environments.commands.some((c) => c.name().includes('workload-mode'))).toBe(false)
  })
})

describe('parseWorkloadIsolation', () => {
  test('accepts process and micro_vm, including the hyphenated spelling', () => {
    expect(parseWorkloadIsolation('process')).toEqual({ isolation: 'process' })
    expect(parseWorkloadIsolation('micro_vm')).toEqual({ isolation: 'micro_vm' })
    expect(parseWorkloadIsolation('Micro-VM')).toEqual({ isolation: 'micro_vm' })
  })

  test('names the accepted values for anything else', () => {
    const result = parseWorkloadIsolation('vm')
    expect('error' in result && result.error).toContain('process, micro_vm')
  })
})

describe('parseWindowDays', () => {
  test('unset leaves the server default', () => {
    expect(parseWindowDays(undefined)).toEqual({})
  })

  test('accepts 1 to 90 and refuses anything else', () => {
    expect(parseWindowDays('30')).toEqual({ days: 30 })
    for (const bad of ['0', '91', '7d', '-1', '1.5']) {
      expect('error' in parseWindowDays(bad)).toBe(true)
    }
  })
})

describe('volume input', () => {
  test('names follow the server rule', () => {
    expect(validVolumeName('data')).toBe(true)
    expect(validVolumeName('pg-data.v2')).toBe(true)
    expect(validVolumeName('a')).toBe(false)
    expect(validVolumeName('-data')).toBe(false)
    expect(validVolumeName('Data')).toBe(false)
    expect(validVolumeName('x'.repeat(65))).toBe(false)
  })

  test('create builds the body and keeps retain explicit', () => {
    expect(buildCreateVolumeRequest({ name: 'data' })).toEqual({ body: { name: 'data', retain_on_delete: false } })
    expect(buildCreateVolumeRequest({ name: 'data', sizeMb: '1024', retain: true })).toEqual({
      body: { name: 'data', retain_on_delete: true, size_mb: 1024 },
    })
  })

  test('create refuses a missing name, a bad name and a bad size', () => {
    expect(buildCreateVolumeRequest({})).toEqual({ error: '--name is required' })
    expect('error' in buildCreateVolumeRequest({ name: 'Bad Name' })).toBe(true)
    expect('error' in buildCreateVolumeRequest({ name: 'data', sizeMb: '0' })).toBe(true)
    expect('error' in buildCreateVolumeRequest({ name: 'data', sizeMb: '1GB' })).toBe(true)
  })

  test('mount paths must be absolute and not the root', () => {
    expect(validateMountPath('/data')).toEqual({ mountPath: '/data' })
    expect('error' in validateMountPath(undefined)).toBe(true)
    expect('error' in validateMountPath('data')).toBe(true)
    expect('error' in validateMountPath('/')).toBe(true)
  })

  test('a volume argument is an id when it is digits, a name otherwise', () => {
    expect(parseVolumeRef('12')).toEqual({ id: 12 })
    expect(parseVolumeRef('data')).toEqual({ name: 'data' })
    expect(parseVolumeRef('12abc')).toEqual({ name: '12abc' })
  })

  test('attachment reads as detached, a path, or a read-only path', () => {
    const base = { id: 1, environment_id: 1, name: 'data', backing: 'named', retain_on_delete: false, created_at: '', updated_at: '' }
    expect(describeAttachment(base as EnvironmentVolumeResponse)).toBe('detached')
    expect(describeAttachment({ ...base, attachment: { mount_path: '/data', read_only: false } } as EnvironmentVolumeResponse)).toBe('/data')
    expect(describeAttachment({ ...base, attachment: { mount_path: '/data', read_only: true } } as EnvironmentVolumeResponse)).toBe('/data (read-only)')
  })
})

describe('resolveRequireLogin', () => {
  test('requires an explicit choice', () => {
    expect('error' in resolveRequireLogin(undefined)).toBe(true)
    expect(resolveRequireLogin(true)).toEqual({ requireLogin: true })
    expect(resolveRequireLogin(false)).toEqual({ requireLogin: false })
  })
})

describe('invalid flags fail before any request', () => {
  test.each([
    [['environments', 'isolation', 'set', 'production', 'vm', '-p', 'app'], 'Unknown isolation'],
    [['environments', 'placement', 'recommend', 'production', '-p', 'app', '--window-days', '0'], '--window-days'],
    [['environments', 'volumes', 'create', 'production', '-p', 'app'], '--name is required'],
    [['environments', 'volumes', 'create', 'production', '-p', 'app', '--name', 'data', '--size-mb', 'big'], '--size-mb'],
    [['environments', 'volumes', 'attach', 'production', 'data', '-p', 'app'], '--mount-path is required'],
    [['environments', 'volumes', 'list', 'production', '-p', 'app', '--page-size', '500'], '--page-size'],
    [['environments', 'ingress', 'set', 'production', '-p', 'app'], '--require-login'],
  ] as const)('%p', async (argv, message) => {
    const result = await run([...argv])
    expect(result.exitCode).toBe(1)
    expect(result.stderr).toContain(message)
    expect(result.fetched).toBe(false)
  })
})
