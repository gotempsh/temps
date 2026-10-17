// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import type {
  DeploymentJobResponse,
  DeploymentResponse,
  ProjectResponse,
} from '@/api/client/types.gen'
import {
  AiAssistantContext,
  type AiAssistantValue,
} from '@/components/ai/AiAssistantContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { DeploymentStages } from './DeploymentStages'

installBrowserWindow()
afterEach(cleanup)

const ASSISTANT: AiAssistantValue = {
  isOpen: false,
  projectId: null,
  openSeq: 0,
  open: () => {},
  close: () => {},
  toggle: () => {},
  pageContext: null,
  setPageContext: () => {},
  currentProject: null,
  setCurrentProject: () => {},
}

function job(id: number, name: string): DeploymentJobResponse {
  return {
    id,
    name,
    job_id: `job-${id}`,
    job_type: 'build',
    log_id: `log-${id}`,
    deployment_id: 7,
    status: 'success',
    execution_order: id,
    created_at: 1,
    updated_at: 1,
  } as DeploymentJobResponse
}

test('each stage toggle has a name and reports whether its logs are shown', async () => {
  const api = mockApi({
    'GET /projects/1/deployments/7/jobs': () =>
      jsonResponse({
        jobs: [job(1, 'Download repository'), job(2, 'Build image')],
        total: 2,
      }),
  })
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <AiAssistantContext.Provider value={ASSISTANT}>
        <DeploymentStages
          project={{ id: 1, slug: 'demo', name: 'demo' } as ProjectResponse}
          deployment={{ id: 7 } as DeploymentResponse}
        />
      </AiAssistantContext.Provider>
    </QueryClientProvider>
  )

  const toggle = await view.findByRole('button', {
    name: 'Show logs for Build image',
  })
  expect(toggle.getAttribute('aria-expanded')).toBe('false')
  expect(toggle.getAttribute('type')).toBe('button')

  fireEvent.click(toggle)
  const expanded = await view.findByRole('button', {
    name: 'Hide logs for Build image',
  })
  expect(expanded.getAttribute('aria-expanded')).toBe('true')
  api.restore()
})
