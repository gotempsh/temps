// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import type { ReactNode } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import {
  getProjectsOptions,
  listAlertsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { AiAssistantProvider } from '@/components/ai/AiAssistantContext'
import { AlertRulesManagement } from '@/components/monitoring/AlertRulesManagement'
import { AiWorkflowsOverview } from './AiWorkflowsOverview'
import MetricAlerts from './MetricAlerts'

function createClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
}

function render(client: QueryClient, node: ReactNode) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <AiAssistantProvider>
          <BreadcrumbProvider>{node}</BreadcrumbProvider>
        </AiAssistantProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

function fail(client: QueryClient, queryKey: QueryKey, error: Error) {
  client
    .getQueryCache()
    .build(client, { queryKey })
    .setState({ status: 'error', error, fetchStatus: 'idle' })
}

const noProjects = { projects: [], total: 0, page: 1, per_page: 50 }

test('Workflows overview with no projects links to project creation', () => {
  const client = createClient()
  client.setQueryData(
    getProjectsOptions({ query: { page: 1, per_page: 50 } }).queryKey,
    noProjects
  )
  const html = render(client, <AiWorkflowsOverview />)
  expect(html).toContain('No projects yet')
  expect(html).toContain('href="/projects/new"')
  expect(html).toContain('Create project')
})

test('Workflows overview is titled distinctly from the AI runtime hub', () => {
  const client = createClient()
  client.setQueryData(
    getProjectsOptions({ query: { page: 1, per_page: 50 } }).queryKey,
    noProjects
  )
  const html = render(client, <AiWorkflowsOverview />)
  expect(html).toContain('>Workflows</h1>')
  expect(html).not.toContain('AI Workflows')
  expect(html).toContain('AI runtime settings')
})

test('error alert rules with no projects links to project creation', () => {
  const client = createClient()
  client.setQueryData(getProjectsOptions().queryKey, noProjects)
  const html = render(client, <AlertRulesManagement />)
  expect(html).toContain('No projects found')
  expect(html).toContain('href="/projects/new"')
  expect(html).not.toContain('Create a project first')
})

test('metric alerts load failure names the cause and offers a retry', () => {
  const client = createClient()
  const project = { id: 7, slug: 'demo', name: 'Demo' } as ProjectResponse
  // The generated client throws the parsed problem body, not an Error.
  fail(
    client,
    listAlertsOptions({ query: { project_id: project.id } }).queryKey,
    {
      type: 'about:blank',
      title: 'Internal Server Error',
      status: 500,
      detail: 'metrics store timed out',
    } as unknown as Error
  )
  const html = render(client, <MetricAlerts project={project} />)
  expect(html).toContain('Failed to load alerts')
  expect(html).toContain(
    'Could not fetch alert rules for Demo: metrics store timed out'
  )
  expect(html).toContain('Retry')
  expect(html).not.toContain('Try refreshing the page')
})
