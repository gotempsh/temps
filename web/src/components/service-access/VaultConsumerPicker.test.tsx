// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import {
  cleanup,
  render,
  fireEvent,
  waitFor,
  within,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { VaultConsumerPicker } from './VaultConsumerPicker'

installBrowserWindow()

test('project consumers show the shared project icon and preserve selection', async () => {
  const api = mockApi({
    'GET /projects': () =>
      jsonResponse({
        projects: [
          {
            id: 7,
            name: 'Fixture project',
            source_type: 'external',
            service_template_image_url: 'https://example.com/project.svg',
          },
        ],
        total: 1,
      }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const selected: (number | string)[] = []
  try {
    render(
      <QueryClientProvider client={client}>
        <VaultConsumerPicker
          kind="project"
          onSelect={(id) => selected.push(id)}
        />
      </QueryClientProvider>
    )
    const ui = within(document.body)
    const button = await ui.findByRole('button', { name: 'Fixture project' })
    await waitFor(() =>
      expect(button.querySelector('img')?.getAttribute('src')).toBe(
        'https://example.com/project.svg'
      )
    )
    fireEvent.click(button)
    expect(selected).toEqual([7])
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})
