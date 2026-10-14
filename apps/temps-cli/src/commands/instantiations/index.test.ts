// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { InstantiationResponse } from '../../api/types.gen.js'
import { runWithApi } from '../../lib/mock-api.js'
import {
  describeMore,
  describeSteps,
  isFinished,
  nextCommands,
  parsePositiveInt,
  registerInstantiationsCommands,
  resolveListPaging,
  toInstantiationPage,
  waitForInstantiation,
  waitTimeoutMs,
} from './index.js'

function instantiation(overrides: Partial<InstantiationResponse> = {}): InstantiationResponse {
  return {
    id: 42,
    kind: 'template',
    name: 'Shop',
    status: 'running',
    created_at: '2026-01-01T00:00:00Z',
    services: [{ name: 'web', slug: 'web', template_service: 'web', project_id: 7 }],
    steps: [
      { key: 'service:web', label: 'Create web', status: 'done' },
      { key: 'deploy:web', label: 'Deploy web', status: 'pending' },
    ],
    ...overrides,
  } as InstantiationResponse
}

describe('flag parsing', () => {
  test('parsePositiveInt accepts whole numbers only and names the flag', () => {
    expect(parsePositiveInt(' 20 ', '--limit')).toBe(20)
    for (const bad of ['abc', '0', '-1', '1.5', '2x', '', 'NaN']) {
      expect(() => parsePositiveInt(bad, '--limit')).toThrow('--limit must be a positive whole number')
    }
  })

  test('--limit is --per-page; conflicting or oversized values are refused', () => {
    expect(resolveListPaging({})).toEqual({ page: 1, perPage: 20 })
    expect(resolveListPaging({ limit: '5' })).toEqual({ page: 1, perPage: 5 })
    expect(resolveListPaging({ page: '3', perPage: '50', limit: '50' })).toEqual({ page: 3, perPage: 50 })
    expect(() => resolveListPaging({ perPage: '10', limit: '20' })).toThrow('pass one of them')
    expect(() => resolveListPaging({ perPage: '101' })).toThrow('at most 100')
    expect(() => resolveListPaging({ page: 'two' })).toThrow('--page')
  })

  test('--timeout is in minutes and defaults to 30', () => {
    expect(waitTimeoutMs()).toBe(30 * 60_000)
    expect(waitTimeoutMs('5')).toBe(5 * 60_000)
    expect(() => waitTimeoutMs('soon')).toThrow('--timeout')
  })
})

describe('list response', () => {
  test('reads a page object or the bare list older servers return', () => {
    const item = instantiation()
    expect(toInstantiationPage([item])).toEqual({ items: [item] })
    expect(toInstantiationPage({ data: [item], total: 3, page: 1, per_page: 1 })).toEqual({ items: [item], total: 3 })
    expect(toInstantiationPage({ items: [item], total: 1 })).toEqual({ items: [item], total: 1 })
    expect(toInstantiationPage({ instantiations: [], total: 0 })).toEqual({ items: [], total: 0 })
    expect(() => toInstantiationPage({ nothing: true })).toThrow('unexpected')
  })

  test('says when there is more than one page', () => {
    expect(describeMore(5, 1, 20, 5)).toBeUndefined()
    expect(describeMore(57, 2, 20, 20)).toBe('Showing 21-40 of 57; next page: --page 3')
    expect(describeMore(57, 3, 20, 17)).toBe('Showing 41-57 of 57')
    // An older server does not say how many there are.
    expect(describeMore(undefined, 1, 20, 20)).toContain('--page 2')
    expect(describeMore(undefined, 1, 20, 3)).toBeUndefined()
  })
})

describe('progress', () => {
  test('a finished instantiation needs no follow-up; others say what to run', () => {
    expect(isFinished({ status: 'succeeded' })).toBeTrue()
    expect(isFinished({ status: 'running' })).toBeFalse()
    expect(nextCommands({ id: 4, status: 'failed' })).toEqual(['bunx @temps-sdk/cli instantiations retry 4'])
    expect(nextCommands({ id: 4, status: 'running' })).toEqual(['bunx @temps-sdk/cli instantiations show 4 --wait'])
    expect(nextCommands({ id: 4, status: 'succeeded' })).toEqual([])
  })

  test('describes each step', () => {
    const lines = describeSteps(
      instantiation({
        steps: [
          { key: 'a', label: 'Create web', status: 'done' },
          { key: 'b', label: 'Deploy web', status: 'failed', error: 'build failed' },
        ],
      })
    )
    expect(lines[0]).toContain('Create web')
    expect(lines[1]).toContain('Deploy web: build failed')
  })

  test('waiting stops at the timeout, says how to follow up, and returns the running state', async () => {
    let loads = 0
    const run = await captureStderr(() =>
      waitForInstantiation(42, {
        timeoutMs: 30,
        intervalMs: 5,
        announce: true,
        load: async () => {
          loads++
          return instantiation()
        },
      })
    )
    expect(run.result.status).toBe('running')
    expect(loads).toBeGreaterThan(1)
    expect(run.output).toContain('instantiations show 42 --wait')
  })

  test('waiting returns as soon as the instantiation finishes', async () => {
    const states = [instantiation(), instantiation({ status: 'succeeded' })]
    const result = await waitForInstantiation(42, {
      timeoutMs: 60_000,
      intervalMs: 1,
      load: async () => states.shift() ?? instantiation({ status: 'succeeded' }),
    })
    expect(result.status).toBe('succeeded')
  })
})

async function captureStderr<T>(fn: () => Promise<T>): Promise<{ result: T; output: string }> {
  const lines: string[] = []
  const original = { warn: console.warn, log: console.log }
  console.warn = (...args: unknown[]) => lines.push(args.join(' '))
  console.log = (...args: unknown[]) => lines.push(args.join(' '))
  try {
    return { result: await fn(), output: lines.join('\n') }
  } finally {
    console.warn = original.warn
    console.log = original.log
  }
}

describe('instantiations list', () => {
  test('a non-numeric --limit fails before any request', async () => {
    const run = await runWithApi(registerInstantiationsCommands, ['instantiations', 'list', '--limit', 'ten'], {})
    expect(run.error?.message).toContain('--limit must be a positive whole number')
    expect(run.requests).toEqual([])
  })

  test('--group takes a project slug and asks for the page', async () => {
    const run = await runWithApi(
      registerInstantiationsCommands,
      ['instantiations', 'list', '--group', 'shop', '--per-page', '2', '--page', '2', '--json'],
      {
        'GET /project-groups/by-slug/shop': { id: 9, name: 'Shop', slug: 'shop', projects: [], created_at: '' },
        'GET /instantiations': { data: [instantiation()], total: 3, page: 2, per_page: 2 },
      }
    )
    expect(run.error).toBeUndefined()
    const list = run.requests.find((r) => r.startsWith('GET /instantiations'))!
    const query = new URLSearchParams(list.split('?')[1])
    expect(query.get('group_id')).toBe('9')
    expect(query.get('page')).toBe('2')
    expect(query.get('per_page')).toBe('2')
    expect(query.get('limit')).toBe('2')
    expect(JSON.parse(run.stdout)).toHaveLength(1)
  })

  test('a numeric --group is used as the id without a lookup', async () => {
    const run = await runWithApi(registerInstantiationsCommands, ['instantiations', 'list', '--group', '9', '--json'], {
      'GET /instantiations': [],
    })
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual(['GET /instantiations?group_id=9&page=1&per_page=20&limit=20'])
  })
})

describe('instantiations show', () => {
  test('a failed instantiation exits non-zero and says how to retry', async () => {
    const run = await runWithApi(registerInstantiationsCommands, ['instantiations', 'get', '42'], {
      'GET /instantiations/42': instantiation({ status: 'failed', error: 'build failed' }),
    })
    expect(run.exitCode).toBe(1)
    expect(run.stdout).toContain('instantiations retry 42')
  })

  test('a bad id fails before any request', async () => {
    const run = await runWithApi(registerInstantiationsCommands, ['instantiations', 'show', 'abc'], {})
    expect(run.error?.message).toContain('not an instantiation id')
    expect(run.requests).toEqual([])
  })
})
