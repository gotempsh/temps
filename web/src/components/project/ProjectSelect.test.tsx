// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ProjectSelect } from './ProjectSelect'

installBrowserWindow()
afterEach(cleanup)

function renderPicker(props: { ariaLabel?: string; value?: number | null }) {
  const api = mockApi({
    'GET /projects': () =>
      jsonResponse({
        projects: [{ id: 3, slug: 'shop', name: 'Shop' }],
        total: 1,
        page: 1,
        per_page: 100,
      }),
  })
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <ProjectSelect
        ariaLabel={props.ariaLabel}
        value={props.value ?? null}
        onValueChange={() => {}}
      />
    </QueryClientProvider>
  )
  return { view, api }
}

// A role="combobox" button takes no name from its content, so without a label
// the picker was an unnamed control (axe `button-name`).
test('names itself after its visible value when no label is given', async () => {
  const { view, api } = renderPicker({})
  const picker = await view.findByRole('combobox', { name: 'All services' })
  expect(picker.getAttribute('aria-label')).toBeNull()
  api.restore()
})

test('an explicit label wins over the visible value', async () => {
  const { view, api } = renderPicker({ ariaLabel: 'Filter by project' })
  const picker = await view.findByRole('combobox', {
    name: 'Filter by project',
  })
  expect(picker.getAttribute('aria-labelledby')).toBeNull()
  api.restore()
})
