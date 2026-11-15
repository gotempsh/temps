// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ProjectSecuritySettings } from './ProjectSecuritySettings'

installBrowserWindow()
afterEach(cleanup)

const project = {
  id: 7,
  slug: 'synthetic-api',
  name: 'synthetic-api',
  shield_mode: 'log',
  ai_alert_summaries_enabled: false,
  ai_api_traffic_summary_enabled: false,
  vulnerability_scanning_enabled: false,
  deployment_config: {},
} as unknown as ProjectResponse

function renderSettings() {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <ProjectSecuritySettings project={project} refetch={() => {}} />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('the page has one Save, enabled by a change in any group and cleared by saving', async () => {
  const saved: unknown[] = []
  mockApi({
    'POST /projects/7/settings': async (request) => {
      saved.push(await request.json())
      return jsonResponse(project)
    },
    'PATCH /projects/7/deployment-config': () => jsonResponse({}),
  })
  const view = renderSettings()

  // One row for the whole form, not one button per group.
  const saves = view.getAllByRole('button', { name: /^Save/ })
  expect(saves).toHaveLength(1)
  const save = saves[0] as HTMLButtonElement
  expect(save.disabled).toBe(true)
  expect(view.getByText('All changes saved')).toBeTruthy()

  fireEvent.click(
    view.getByRole('switch', { name: 'Enable vulnerability scanning' })
  )
  expect(save.disabled).toBe(false)
  expect(view.getByText('You have unsaved changes')).toBeTruthy()

  fireEvent.click(save)
  await waitFor(() => expect(saved).toHaveLength(1))
  expect(saved[0]).toEqual({ vulnerability_scanning_enabled: true })
  await waitFor(() => expect(save.disabled).toBe(true))
  expect(view.getByText('All changes saved')).toBeTruthy()
})
