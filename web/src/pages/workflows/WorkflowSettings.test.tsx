// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type { WorkflowSettings } from '@/api/client'
import { SettingsForm } from './WorkflowSettings'

test('drain is explicitly opt-in and explains suspended runs and timeout behavior', () => {
  const settings = {
    drain_timeout_secs: 0,
    host_service: null,
  } as WorkflowSettings
  const html = renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider client={new QueryClient()}>
        <SettingsForm
          path={{ project_id: 1, environment_id: 2 }}
          settings={settings}
        />
      </QueryClientProvider>
    </MemoryRouter>
  )
  expect(html).toContain('Journal storage')
  expect(html).toContain('existing runs keep their original store')
  expect(html).toContain('/projects/1/storage')
  expect(html).toContain('0 disables drain')
  expect(html).toContain('old deployment remains active')
  expect(html).toContain('Sleeping and waiting runs resume on the new code')
  expect(html).toContain('min="0" max="3600"')
})
