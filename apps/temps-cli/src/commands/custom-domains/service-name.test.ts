// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { buildCustomDomainUpdateBody, registerCustomDomainsCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('custom domains --service', () => {
  test('create routes the domain to a Compose service', async () => {
    const run = await runWithApi(
      registerCustomDomainsCommands,
      ['custom-domains', 'create', '--project-id', '7', '-d', 'shop.example.com', '--environment-id', '2', '--service', 'web', '--yes'],
      { 'POST /projects/7/custom-domains': { id: 1, domain: 'shop.example.com', status: 'pending' } },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toMatchObject({ domain: 'shop.example.com', service_name: 'web' })
  })

  test('update sets or clears the service', () => {
    expect(buildCustomDomainUpdateBody({ service: 'api' })).toEqual({ service_name: 'api' })
    expect(buildCustomDomainUpdateBody({ service: '' })).toEqual({ service_name: null })
    expect(buildCustomDomainUpdateBody({})).toEqual({})
  })
})
