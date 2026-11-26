// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerLoadBalancerCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('load-balancer route type', () => {
  test('create and update send --type', async () => {
    const created = await runWithApi(registerLoadBalancerCommands, ['load-balancer', 'create', '-d', 'db.example.com', '-t', 'http://10.0.0.5:5432', '--type', 'tls', '--yes'], {
      'POST /lb/routes': { domain: 'db.example.com' },
    })
    expect(created.error).toBeUndefined()
    expect(created.bodies[0]?.body).toMatchObject({ domain: 'db.example.com', route_type: 'tls' })
    const updated = await runWithApi(registerLoadBalancerCommands, ['load-balancer', 'update', '-d', 'db.example.com', '-t', 'http://10.0.0.6:5432', '--type', 'http'], {
      'PUT /lb/routes/db.example.com': { domain: 'db.example.com' },
    })
    expect(updated.error).toBeUndefined()
    expect(updated.bodies[0]?.body).toMatchObject({ route_type: 'http' })
  })

  test('an unknown type fails before any request', async () => {
    const run = await runWithApi(registerLoadBalancerCommands, ['load-balancer', 'create', '-d', 'a.example.com', '-t', 'http://h:1', '--type', 'udp', '--yes'], {})
    expect(run.error?.message).toMatch(/--type must be one of http, tls/)
    expect(run.requests).toEqual([])
  })
})
