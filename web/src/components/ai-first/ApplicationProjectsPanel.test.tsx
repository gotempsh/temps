// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  listProjectServicesOptions,
  listServicesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { ApplicationResponse, ExternalServiceInfo } from '@/api/client'
import { ApplicationProjectsPanel } from './ApplicationProjectsPanel'

describe('ApplicationProjectsPanel', () => {
  test('renders primary, deployment, and automatic-deploy state', () => {
    const queryClient = new QueryClient()
    const html = renderToStaticMarkup(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter>
          <ApplicationProjectsPanel
            application={application}
            onApplicationChange={() => {}}
          />
        </MemoryRouter>
      </QueryClientProvider>
    )

    expect(html).toContain('Primary')
    expect(html).toContain('Not deployed yet')
    expect(html).toContain('Automatic deploy')
    expect(html).toContain('Disabled')
    expect(html).toContain('Production')
    expect(html).toContain('ready')
    expect(html).toContain('/projects/web')
    expect(html).toContain('An application must keep at least one project')
    expect(html).toContain('Project topology is injected fresh')
  })

  test('shows linked databases and creates new ones atomically for the primary project', () => {
    const queryClient = new QueryClient()
    queryClient.setQueryData(listServicesOptions().queryKey, [postgres])
    queryClient.setQueryData(
      listProjectServicesOptions({ path: { project_id: 7 } }).queryKey,
      [
        {
          id: 12,
          project: {
            id: 7,
            slug: 'web',
            created_at: '2026-09-03T00:00:00Z',
          },
          service: postgres,
        },
      ]
    )

    const html = renderToStaticMarkup(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter>
          <ApplicationProjectsPanel
            application={application}
            onApplicationChange={() => {}}
          />
        </MemoryRouter>
      </QueryClientProvider>
    )

    expect(html).toContain('Databases')
    expect(html).toContain('main-postgres')
    expect(html).toContain('PostgreSQL 18 · private network')
    expect(html).toContain('/storage/create?project_id=7')
    expect(html).toContain('sandbox receives no reusable platform token')
  })

  test('requires a project before offering database links', () => {
    const queryClient = new QueryClient()
    const html = renderToStaticMarkup(
      <QueryClientProvider client={queryClient}>
        <MemoryRouter>
          <ApplicationProjectsPanel
            application={{ ...application, projects: [] }}
            onApplicationChange={() => {}}
          />
        </MemoryRouter>
      </QueryClientProvider>
    )

    expect(html).toContain('Add a project first')
    expect(html).toContain(
      'Databases are linked through an application project'
    )
    expect(html).not.toContain('Choose a database')
  })
})

const application: ApplicationResponse = {
  public_id: 'app_test',
  name: 'Test application',
  description: null,
  status: 'active',
  created_at: '2026-09-03T00:00:00Z',
  updated_at: '2026-09-03T00:00:00Z',
  projects: [
    {
      id: 7,
      name: 'Web',
      slug: 'web',
      repository: '/',
      main_branch: 'main',
      is_private: true,
      is_primary: true,
      automatic_deploy: false,
      last_deployment_at: null,
      environments: [
        {
          name: 'Production',
          slug: 'production',
          sleeping: false,
          deployment_state: 'ready',
        },
      ],
    },
  ],
}

const postgres: ExternalServiceInfo = {
  id: 4,
  name: 'main-postgres',
  service_type: 'postgres',
  status: 'running',
  topology: 'standalone',
  version: '18',
  created_at: '2026-09-03T00:00:00Z',
  updated_at: '2026-09-03T00:00:00Z',
}
