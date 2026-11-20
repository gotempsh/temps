// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { RepositoryResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { PresetContext } from '@/contexts/PresetContext-shared'
import { ProjectConfigurator } from './ProjectConfigurator'

installBrowserWindow()
afterEach(cleanup)

function setup(preset = 'dockerfile') {
  const requests: Record<string, unknown>[] = []
  const api = mockApi({
    'GET /deployments/managed-environment-variables': () => jsonResponse([]),
    'GET /external-services': () => jsonResponse([]),
    'GET /presets': () => jsonResponse([]),
    'POST /projects': async (request) => {
      requests.push(await request.json())
      return jsonResponse({ id: 5, slug: 'worker', name: 'worker' })
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <PresetContext.Provider
          value={{
            presets: [],
            isLoading: false,
            error: null,
            getPresetBySlug: () => undefined,
          }}
        >
          <ProjectConfigurator
            repository={
              {
                id: 1,
                name: 'worker',
                owner: 'example',
                default_branch: 'main',
              } as RepositoryResponse
            }
            branches={[]}
            presetData={[]}
            defaultValues={{ name: 'worker', preset }}
          />
        </PresetContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { view, requests, api, client }
}

test('creates a Dockerfile service with startup arguments before its first deployment', async () => {
  const { view, requests, api, client } = setup()
  try {
    fireEvent.click(view.getByRole('button', { name: /Advanced settings/ }))
    fireEvent.change(view.getByLabelText('Command (CMD)'), {
      target: { value: '["worker", "argument with spaces", ""]' },
    })
    fireEvent.change(view.getByLabelText('Entrypoint (ENTRYPOINT)'), {
      target: { value: '[]' },
    })
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50))
    })
    fireEvent.click(view.getByRole('button', { name: 'Create service' }))
    await waitFor(() => expect(requests.length).toBe(1))
    expect(requests[0].preset_config).toEqual({
      dockerfilePath: 'Dockerfile',
      command: ['worker', 'argument with spaces', ''],
      entrypoint: [],
    })
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})

test('rejects invalid startup arguments without sending a creation request', async () => {
  const { view, requests, api, client } = setup()
  try {
    fireEvent.click(view.getByRole('button', { name: /Advanced settings/ }))
    fireEvent.change(view.getByLabelText('Command (CMD)'), {
      target: { value: 'python worker.py' },
    })
    fireEvent.click(view.getByRole('button', { name: /Advanced settings/ }))
    fireEvent.click(view.getByRole('button', { name: 'Create service' }))
    await view.findByText(/Enter a JSON array, for example/)
    expect(requests).toEqual([])
    expect(
      (view.getByLabelText('Command (CMD)') as HTMLTextAreaElement).value
    ).toBe('python worker.py')
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})

test('Dockerfile startup controls are not shown for a different build preset', () => {
  const { view, api, client } = setup('python')
  try {
    expect(view.queryByRole('button', { name: /Advanced settings/ })).toBeNull()
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})

test('leaving advanced settings untouched inherits the image startup defaults', async () => {
  const { view, requests, api, client } = setup()
  try {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50))
    })
    fireEvent.click(view.getByRole('button', { name: 'Create service' }))
    await waitFor(() => expect(requests.length).toBe(1))
    expect(requests[0].preset_config).toEqual({
      dockerfilePath: 'Dockerfile',
      command: null,
      entrypoint: null,
    })
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})
