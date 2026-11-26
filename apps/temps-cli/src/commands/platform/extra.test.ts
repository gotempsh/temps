// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { operationsQuery, staticFilePath } from './extra.js'
import { registerPlatformCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('platform extras', () => {
  test('operationsQuery validates status and kind', () => {
    expect(operationsQuery({ status: 'running', kind: 'backup', project: '3', page: '1', pageSize: '5' })).toEqual({
      status: 'running',
      kind: 'backup',
      project_id: 3,
      page: 1,
      page_size: 5,
    })
    expect(() => operationsQuery({ status: 'done' })).toThrow(/--status/)
    expect(() => operationsQuery({ kind: 'deploy' })).toThrow(/--kind/)
  })

  test('staticFilePath refuses traversal', () => {
    expect(staticFilePath('/img/logo.png')).toBe('img/logo.png')
    expect(() => staticFilePath('../etc/passwd')).toThrow(/not a path/)
  })

  test.each([
    [['operations', '--status', 'all'], 'GET /operations?status=all', { operations: [], total: 0, running_count: 0, page: 1, page_size: 20 }],
    [['maturity'], 'GET /v1/platform/feature-maturity', []],
    [['geo', '192.0.2.1'], 'GET /geo/192.0.2.1', { ip: '192.0.2.1', is_eu: false }],
  ])('platform %p', async (argv, request, body) => {
    const run = await runWithApi(registerPlatformCommands, ['platform', ...argv, '--json'], { [request.split('?')[0] as string]: body })
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual([request])
  })

  test('file keeps nested slashes literal and prints the content', async () => {
    const run = await runWithApi(registerPlatformCommands, ['platform', 'file', 'docs/a b.txt', '-o', '/dev/null'], {
      'GET /files/docs/a%20b.txt': new Response('hello'),
    })
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual(['GET /files/docs/a%20b.txt'])
  })
})
