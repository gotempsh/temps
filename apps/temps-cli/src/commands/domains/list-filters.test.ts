// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerDomainsCommands } from './index.js'
import { registerNotificationsCommands } from '../notifications/index.js'
import { registerWebhooksCommands } from '../webhooks/index.js'
import { registerAlarmsCommands } from '../alarms/index.js'
import { registerTeamsCommands } from '../teams/index.js'
import { registerTemplatesCommands } from '../templates/index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('list commands pass every query parameter', () => {
  test.each([
    [registerDomainsCommands, ['domains', 'list', '--search', 'shop', '--sort-by', 'domain', '--sort-order', 'asc'], 'GET /domains', 'GET /domains?search=shop&sort_by=domain&sort_order=asc', { domains: [] }],
    [registerDomainsCommands, ['domains', 'orders', 'list', '--sort-by', 'status'], 'GET /orders', 'GET /orders?sort_by=status', { orders: [] }],
    [registerNotificationsCommands, ['notifications', 'list', '--sort-order', 'asc'], 'GET /notification-providers', 'GET /notification-providers?sort_order=asc', []],
    [registerNotificationsCommands, ['notifications', 'routes', 'list', '--sort-by', 'name'], 'GET /notification-routes', 'GET /notification-routes?page=1&page_size=100&sort_by=name', { items: [], total: 0 }],
    [registerWebhooksCommands, ['webhooks', 'list', '--project-id', '7', '--page', '2', '--page-size', '50'], 'GET /projects/7/webhooks', 'GET /projects/7/webhooks?page=2&page_size=50', []],
    [registerTeamsCommands, ['teams', 'list', '--page', '2'], 'GET /teams', 'GET /teams?page_size=100&page=2', { teams: [] }],
    [registerTemplatesCommands, ['templates', 'list', '--featured', '--tag', 'nextjs'], 'GET /templates', 'GET /templates?featured=true&tag=nextjs', []],
  ] as const)('%#', async (register, argv, route, request, body) => {
    const run = await runWithApi(register, [...argv, '--json'], { [route]: body })
    expect(run.error).toBeUndefined()
    expect(run.requests).toContain(request)
  })

  test('alarms list filters by external service', async () => {
    const run = await runWithApi(registerAlarmsCommands, ['alarms', 'list', '--system', '--service-id', '4', '--json'], {
      'GET /system/alarms': { items: [], total: 0, page: 1, page_size: 20 },
    })
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual(['GET /system/alarms?service_id=4'])
  })

  test('an invalid sort column fails before any request', async () => {
    const run = await runWithApi(registerDomainsCommands, ['domains', 'list', '--sort-by', 'size'], {})
    expect(run.error?.message).toMatch(/--sort-by must be one of/)
    expect(run.requests).toEqual([])
  })
})
