// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { getWorkflowsOverviewOptions } from '@/api/client/@tanstack/react-query.gen'
import { WorkflowEnvironment } from './EnvironmentWorkflows'

test('an environment without runs exposes onboarding, grants, keys and drain settings', () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData(
    getWorkflowsOverviewOptions({ path: { project_id: 1, environment_id: 2 } })
      .queryKey,
    {
      executor: {
        replicas: 0,
        configured: false,
        reason: 'No deployment is live',
        setup_path: '/docs/workflows/getting-started',
      },
      credential: { configured: false },
      workflows: [],
    }
  )
  const html = renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <WorkflowEnvironment projectId={1} environmentId={2} />
      </QueryClientProvider>
    </MemoryRouter>
  )
  expect(html).toContain('No deployment is live')
  expect(html).not.toContain('/docs/workflows/getting-started')
  expect(html).toContain('href="/workflows/1/2/grants"')
  expect(html).toContain('href="/workflows/1/2/settings"')
  expect(html).toContain('href="/workflows/1/keys"')
  expect(html).toContain('Create credential')
  expect(html).toContain('Runs stay queued')
  expect(html).toContain('Start run')
  expect(html).not.toContain('Copy this credential now')
})
