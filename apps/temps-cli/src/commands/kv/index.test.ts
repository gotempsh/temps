// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { buildEnableRequest, buildSetOptions, describeTtl, parseKvValue, registerKvCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

const project = { 'GET /projects/by-slug/web': { id: 7, slug: 'web', name: 'web' } }

describe('kv helpers', () => {
  test('parseKvValue keeps strings and parses JSON only when asked', () => {
    expect(parseKvValue('42', false)).toBe('42')
    expect(parseKvValue('42', true)).toBe(42)
    expect(parseKvValue('{"a":[1]}', true)).toEqual({ a: [1] })
    expect(() => parseKvValue('{nope', true)).toThrow(/not JSON/)
  })

  test('buildSetOptions converts expiry and conditions, refusing contradictions', () => {
    expect(buildSetOptions({})).toEqual({})
    expect(buildSetOptions({ ex: '60', nx: true })).toEqual({ ex: 60, nx: true })
    expect(buildSetOptions({ px: '1500', xx: true })).toEqual({ px: 1500, xx: true })
    expect(() => buildSetOptions({ ex: '1', px: '1' })).toThrow(/not both/)
    expect(() => buildSetOptions({ nx: true, xx: true })).toThrow(/not both/)
    expect(() => buildSetOptions({ ex: '0' })).toThrow(/--ex must be/)
    expect(() => buildSetOptions({ ex: 'soon' })).toThrow(/--ex must be/)
  })

  test('buildEnableRequest sends only given fields and checks the memory size', () => {
    expect(buildEnableRequest({})).toEqual({})
    expect(buildEnableRequest({ maxMemory: '256mb', persistence: false, image: 'redis:8' })).toEqual({
      max_memory: '256mb',
      persistence: false,
      docker_image: 'redis:8',
    })
    expect(() => buildEnableRequest({ maxMemory: 'lots' })).toThrow(/--max-memory/)
  })

  test('describeTtl names the special values', () => {
    expect(describeTtl(-2)).toMatch(/does not exist/)
    expect(describeTtl(-1)).toBe('no expiration')
    expect(describeTtl(30)).toBe('30s')
  })
})

describe('temps kv', () => {
  test('get resolves the project slug and prints the value', async () => {
    const run = await runWithApi(registerKvCommands, ['kv', 'get', 'greeting', '-p', 'web'], {
      ...project,
      'POST /kv/get': { value: 'hello' },
    })
    expect(run.error).toBeUndefined()
    expect(run.bodies).toEqual([{ request: 'POST /kv/get', body: { key: 'greeting', project_id: 7 } }])
    expect(run.stdout).toContain('hello')
  })

  test('get of a missing key exits non-zero', async () => {
    const run = await runWithApi(registerKvCommands, ['kv', 'get', 'missing', '-p', '7'], {
      'POST /kv/get': { value: null },
    })
    expect(run.error).toBeUndefined()
    expect(run.exitCode).toBe(1)
    // A numeric --project is the ID: no slug lookup.
    expect(run.requests).toEqual(['POST /kv/get'])
  })

  test('get of a stored JSON null succeeds; a missing key with exists:false fails', async () => {
    const stored = await runWithApi(registerKvCommands, ['kv', 'get', 'nullable', '-p', '7', '--json'], {
      'POST /kv/get': { value: null, exists: true },
    })
    expect(stored.error).toBeUndefined()
    expect(stored.exitCode).not.toBe(1)
    expect(JSON.parse(stored.stdout)).toEqual({ key: 'nullable', value: null, exists: true })
    const missing = await runWithApi(registerKvCommands, ['kv', 'get', 'gone', '-p', '7'], {
      'POST /kv/get': { value: null, exists: false },
    })
    expect(missing.exitCode).toBe(1)
    expect(missing.stdout).toContain('does not exist')
  })

  test('a failure exits non-zero with --json too: missing key, unmet set condition, expire of a missing key', async () => {
    const routes = { 'POST /kv/get': { value: null }, 'POST /kv/set': { result: null }, 'POST /kv/expire': { success: false } }
    for (const argv of [
      ['kv', 'get', 'missing'],
      ['kv', 'set', 'k', 'v', '--nx'],
      ['kv', 'expire', 'k', '30'],
    ]) {
      const run = await runWithApi(registerKvCommands, [...argv, '-p', '7', '--json'], routes)
      expect(run.error).toBeUndefined()
      expect(run.exitCode).toBe(1)
      expect(() => JSON.parse(run.stdout)).not.toThrow()
    }
  })

  test('an all-digit slug from the environment is looked up, not taken as an ID', async () => {
    const savedProject = process.env.TEMPS_PROJECT
    process.env.TEMPS_PROJECT = '2024'
    try {
      const run = await runWithApi(registerKvCommands, ['kv', 'get', 'k', '--json'], {
        'GET /projects/by-slug/2024': { id: 31, slug: '2024', name: '2024' },
        'POST /kv/get': { value: 'v' },
      })
      expect(run.error).toBeUndefined()
      expect(run.requests).toEqual(['GET /projects/by-slug/2024', 'POST /kv/get'])
      expect(run.bodies[0]?.body).toEqual({ key: 'k', project_id: 31 })
    } finally {
      if (savedProject === undefined) delete process.env.TEMPS_PROJECT
      else process.env.TEMPS_PROJECT = savedProject
    }
  })

  test('set sends the JSON value with expiry and condition', async () => {
    const run = await runWithApi(
      registerKvCommands,
      ['kv', 'set', 'counter', '{"n":1}', '--json-value', '--ex', '60', '--nx', '-p', '7', '--json'],
      { 'POST /kv/set': { result: 'OK' } },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ key: 'counter', value: { n: 1 }, project_id: 7, ex: 60, nx: true })
  })

  test('set reads the value from stdin', async () => {
    const run = await runWithApi(
      registerKvCommands,
      ['kv', 'set', 'token', '--value-stdin', '-p', '7'],
      { 'POST /kv/set': { result: 'OK' } },
      { stdin: 'from-a-pipe' },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toMatchObject({ key: 'token', value: 'from-a-pipe' })
  })

  test('set without a value fails before calling the API', async () => {
    const run = await runWithApi(registerKvCommands, ['kv', 'set', 'token', '-p', '7'], {})
    expect(run.error).toBeDefined()
    expect(run.requests).toEqual([])
  })

  test('del asks first: without a terminal it refuses and deletes nothing', async () => {
    const run = await runWithApi(
      registerKvCommands,
      ['kv', 'del', 'a', 'b', '-p', '7'],
      { 'POST /kv/del': { deleted: 2 } },
      { stdin: '' },
    )
    expect(run.error?.message).toMatch(/not an interactive terminal/)
    expect(run.requests).toEqual([])
  })

  test('del --yes deletes every key given', async () => {
    const run = await runWithApi(registerKvCommands, ['kv', 'del', 'a', 'b', '-p', '7', '--yes', '--json'], {
      'POST /kv/del': { deleted: 1 },
    })
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ keys: ['a', 'b'], project_id: 7 })
    expect(JSON.parse(run.stdout)).toEqual({ deleted: 1 })
  })

  test('keys defaults the pattern to *', async () => {
    const run = await runWithApi(registerKvCommands, ['kv', 'keys', '-p', '7', '--json'], {
      'POST /kv/keys': { keys: ['a'] },
    })
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ pattern: '*', project_id: 7 })
  })

  test('ttl, expire and incr send their fields', async () => {
    const routes = {
      'POST /kv/ttl': { ttl: 10 },
      'POST /kv/expire': { success: false },
      'POST /kv/incr': { value: 5 },
    }
    const ttl = await runWithApi(registerKvCommands, ['kv', 'ttl', 'k', '-p', '7', '--json'], routes)
    expect(JSON.parse(ttl.stdout)).toEqual({ key: 'k', ttl: 10 })
    const expire = await runWithApi(registerKvCommands, ['kv', 'expire', 'k', '30', '-p', '7'], routes)
    expect(expire.bodies[0]?.body).toEqual({ key: 'k', seconds: 30, project_id: 7 })
    expect(expire.exitCode).toBe(1)
    const incr = await runWithApi(registerKvCommands, ['kv', 'incr', 'k', '--by', '-2', '-p', '7'], routes)
    expect(incr.bodies[0]?.body).toEqual({ key: 'k', project_id: 7, amount: -2 })
    expect(incr.stdout).toContain('5')
  })

  test('status --json prints the service status', async () => {
    const status = { enabled: true, healthy: true, docker_image: 'redis:8', version: '8.0' }
    const run = await runWithApi(registerKvCommands, ['kv', 'status', '--json'], { 'GET /kv/status': status })
    expect(JSON.parse(run.stdout)).toEqual(status)
  })

  test('enable sends the options; disable needs confirmation', async () => {
    const enable = await runWithApi(registerKvCommands, ['kv', 'enable', '--max-memory', '1gb', '--persistence'], {
      'POST /kv/enable': { success: true, message: 'KV enabled', status: {} },
    })
    expect(enable.error).toBeUndefined()
    expect(enable.bodies[0]?.body).toEqual({ max_memory: '1gb', persistence: true })
    const disable = await runWithApi(
      registerKvCommands,
      ['kv', 'disable'],
      { 'DELETE /kv/disable': { success: true, message: 'off' } },
      { stdin: '' },
    )
    expect(disable.requests).toEqual([])
    const confirmed = await runWithApi(registerKvCommands, ['kv', 'disable', '--yes'], {
      'DELETE /kv/disable': { success: true, message: 'off' },
    })
    expect(confirmed.requests).toEqual(['DELETE /kv/disable'])
  })

  test('update --yes sends the image', async () => {
    const run = await runWithApi(registerKvCommands, ['kv', 'update', '--image', 'redis:8.1', '--yes'], {
      'PATCH /kv/update': { success: true, message: 'updated', status: {} },
    })
    expect(run.bodies[0]?.body).toEqual({ docker_image: 'redis:8.1' })
  })

  test('container-name shows the setting, and checks the required prefix before saving', async () => {
    const settings = {
      container_name: 'redis-temps-kv-1',
      default_container_name: 'redis-temps-kv-1',
      custom: false,
      in_use: false,
      required_prefix: 'redis-',
    }
    const show = await runWithApi(registerKvCommands, ['kv', 'container-name', '--json'], { 'GET /kv/settings': settings })
    expect(JSON.parse(show.stdout)).toEqual(settings)
    const bad = await runWithApi(registerKvCommands, ['kv', 'container-name', '--container-name', 'kv-b'], {
      'GET /kv/settings': settings,
    })
    expect(bad.error?.message).toMatch(/must start with "redis-"/)
    expect(bad.requests).toEqual(['GET /kv/settings'])
    const reset = await runWithApi(registerKvCommands, ['kv', 'container-name', '--reset'], {
      'GET /kv/settings': settings,
      'PATCH /kv/settings': settings,
    })
    expect(reset.bodies).toEqual([{ request: 'PATCH /kv/settings', body: { container_name: '' } }])
  })
})
