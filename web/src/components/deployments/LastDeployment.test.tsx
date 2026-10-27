// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type { DeploymentResponse } from '@/api/client'
import { LastDeployment } from './LastDeployment'

function deployment(
  environment: Partial<DeploymentResponse['environment']>
): DeploymentResponse {
  return {
    id: 7,
    project_id: 1,
    environment_id: 1,
    is_current: true,
    status: 'completed',
    created_at: Date.parse('2026-01-01T00:00:00Z'),
    url: 'http://shop-production.localho.st:8080',
    commit_message: 'Ship it',
    environment: {
      id: 1,
      name: 'production',
      slug: 'production',
      domains: ['https://shop.example.com'],
      primary_url: null,
      no_address_reason: null,
      ...environment,
    },
  } as DeploymentResponse
}

function render(value: DeploymentResponse) {
  return renderToStaticMarkup(
    <MemoryRouter>
      <LastDeployment deployment={value} projectName="shop" />
    </MemoryRouter>
  )
}

test('the primary address is shown and opened', () => {
  const html = render(
    deployment({ primary_url: 'https://shop-production.tunnel.example.com' })
  )
  expect(html).toContain('href="https://shop-production.tunnel.example.com"')
  expect(html).toContain('Open site')
})

test('an environment nobody can reach shows why, and no URL to open', () => {
  const html = render(
    deployment({
      no_address_reason:
        'shop-production.localho.st only resolves on this machine',
    })
  )
  expect(html).toContain(
    'Nobody can open it yet: shop-production.localho.st only resolves on this machine'
  )
  expect(html).not.toContain('href="https://shop.example.com"')
  expect(html).not.toContain('localho.st:8080')
  expect(html).toMatch(/<button[^>]*disabled[^>]*>Open site/)
})

test('public Compose service URLs are hidden too when nobody can reach the environment', () => {
  const html = render(
    deployment({
      no_address_reason: 'nothing serves it publicly',
      service_urls: [
        {
          service: 'api',
          port: 3001,
          url: 'http://shop-production--api.localho.st:8080',
        },
      ] as DeploymentResponse['environment']['service_urls'],
    })
  )
  expect(html).toContain('Nobody can open it yet: nothing serves it publicly')
  expect(html).not.toContain('api:3001')
  expect(html).not.toContain('shop-production--api.localho.st')
})
