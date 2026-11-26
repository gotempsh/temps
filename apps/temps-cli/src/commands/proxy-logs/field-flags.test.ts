// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerProxyLogsCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('proxy-logs filters', () => {
  test('list sends the exact path, visitor and AI agent filters', async () => {
    const run = await runWithApi(
      registerProxyLogsCommands,
      ['proxy-logs', 'list', '--path-exact', '/checkout', '--visitor-id', '12', '--exclude-bots', '--no-is-ai-agent', '--ai-agent', 'GPTBot', '--json'],
      { 'GET /proxy-logs': { logs: [], total: 0, page: 1, page_size: 20, total_pages: 0 } },
    )
    expect(run.error).toBeUndefined()
    const url = new URL(`http://x${run.requests[0]?.slice(4)}`)
    expect(Object.fromEntries(url.searchParams)).toMatchObject({
      path_exact: '/checkout',
      visitor_id: '12',
      exclude_bots: 'true',
      is_ai_agent: 'false',
      ai_agent: 'GPTBot',
    })
  })

  test('stats and today filter by status class; a bad class fails before any request', async () => {
    const stats = await runWithApi(registerProxyLogsCommands, ['proxy-logs', 'stats', '--status-code-class', '5xx', '--has-project', '--json'], {
      'GET /proxy-logs/stats/time-buckets': [],
    })
    expect(stats.error).toBeUndefined()
    expect(stats.requests[0]).toContain('status_code_class=5xx')
    expect(stats.requests[0]).toContain('has_project=true')
    const today = await runWithApi(registerProxyLogsCommands, ['proxy-logs', 'today', '--status-code-class', '4xx', '--json'], {
      'GET /proxy-logs/stats/today': { total_requests: 0 },
    })
    expect(today.error).toBeUndefined()
    expect(today.requests[0]).toContain('status_code_class=4xx')
    const bad = await runWithApi(registerProxyLogsCommands, ['proxy-logs', 'today', '--status-code-class', '6xx'], {})
    expect(bad.error?.message).toMatch(/--status-code-class must be one of 2xx, 3xx, 4xx, 5xx/)
    expect(bad.requests).toEqual([])
  })
})
