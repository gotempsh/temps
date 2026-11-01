// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { AiAssistantContext } from '@/components/ai/AiAssistantContext-shared'
import type { AiAssistantValue } from '@/components/ai/AiAssistantContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { AutomationSettings } from './AutomationSettings'

installBrowserWindow()
afterEach(cleanup)

test('crash autofix is shown off, not on, when its setting did not load', async () => {
  const api = mockApi({
    'GET /projects/17/autofix': () =>
      jsonResponse({ title: 'Unavailable', status: 503 }, 503),
    'GET /projects/17/agents': () =>
      jsonResponse({ items: [], total: 0, page: 1, page_size: 20 }),
  })
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <AiAssistantContext.Provider
          value={{ openPage: () => {} } as unknown as AiAssistantValue}
        >
          <AutomationSettings
            project={
              {
                id: 17,
                slug: 'synthetic',
                name: 'Synthetic',
              } as ProjectResponse
            }
          />
        </AiAssistantContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )

  const toggle = await view.findByRole('switch', {
    name: 'Run Crash autofix automatically',
  })
  expect(toggle.getAttribute('aria-checked')).toBe('false')
  expect(toggle.hasAttribute('disabled')).toBe(true)
  api.restore()
})
