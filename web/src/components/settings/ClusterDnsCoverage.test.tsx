// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type { DeploymentWithoutResolver } from '@/api/client'
import {
  ClusterDnsCoverageRows,
  ClusterDnsStaleAlert,
} from './ClusterDnsCoverage'
import { staleBannerMessage } from '@/lib/cluster-dns-coverage'

const entry: DeploymentWithoutResolver = {
  deployment_id: 41,
  project_id: 3,
  project_name: 'Shop',
  project_slug: 'shop',
  environment_id: 9,
  environment_name: 'production',
  hosts: [{ node_id: 2, node_name: 'worker-b' }],
}

describe('ClusterDnsCoverageRows', () => {
  test('names each application, where it runs, and offers a redeploy', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <ClusterDnsCoverageRows entries={[entry]} onRedeploy={() => {}} />
      </MemoryRouter>
    )
    expect(html).toContain('href="/projects/shop"')
    expect(html).toContain('Shop')
    expect(html).toContain('production')
    expect(html).toContain('Running on worker-b')
    expect(html).toContain('Redeploy')
  })
})

describe('ClusterDnsStaleAlert', () => {
  test('explains why a redeploy is needed and links to the settings', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <ClusterDnsStaleAlert entries={[entry]} onRedeploy={() => {}} />
      </MemoryRouter>
    )
    expect(html).toContain('The production environment was started before')
    expect(html).toContain('Redeploy production')
    expect(html).toContain('href="/settings/nodes#cluster-dns"')
  })
})

describe('staleBannerMessage', () => {
  test('names every environment that needs a redeploy', () => {
    expect(
      staleBannerMessage(['staging', 'preview', 'production'])
    ).toStartWith(
      'The staging, preview and production environments were started before'
    )
    expect(staleBannerMessage(['staging', 'production'])).toContain(
      'Redeploy them to fix this.'
    )
  })
})
