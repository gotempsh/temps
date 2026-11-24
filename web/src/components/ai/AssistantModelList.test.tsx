// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { getProviderKeyQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import { AssistantModelList } from './AssistantModelPicker'

installBrowserWindow()
afterEach(cleanup)

function renderList(onChange: (modelId: string) => void) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData(getProviderKeyQueryKey({ path: { id: 7 } }), {
    models: [
      {
        model_id: 'vendor/model-a',
        display_name: 'Model A',
        is_available: true,
        is_enabled: true,
      },
    ],
  } as never)
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <AssistantModelList
          providerKeyId={7}
          value={null}
          onChange={onChange}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('an unlisted model id points to refreshing the key, and cannot be chosen', () => {
  const chosen: string[] = []
  const view = renderList((modelId) => chosen.push(modelId))
  fireEvent.change(view.getByLabelText('Search models'), {
    target: { value: 'vendor/model-next' },
  })
  expect(view.getByText(/No model matches/)).toBeTruthy()
  const link = view.getByRole('link', { name: 'AI Gateway' })
  expect(link.getAttribute('href')).toBe('/ai-gateway')
  // A gateway key only serves the models its operator enabled: no row
  // offers the typed id.
  expect(view.queryByText(/^Use “/)).toBeNull()
  for (const button of view.queryAllByRole('button')) {
    expect(button.textContent ?? '').not.toContain('vendor/model-next')
  }
  expect(chosen).toEqual([])
})

test('a listed model is still found and chosen', () => {
  const chosen: string[] = []
  const view = renderList((modelId) => chosen.push(modelId))
  fireEvent.change(view.getByLabelText('Search models'), {
    target: { value: 'model-a' },
  })
  fireEvent.click(view.getByRole('button', { name: /Model A/ }))
  expect(chosen).toEqual(['vendor/model-a'])
})
