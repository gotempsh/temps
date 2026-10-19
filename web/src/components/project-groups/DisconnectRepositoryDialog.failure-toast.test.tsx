// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, spyOn, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { toast } from 'sonner'
import type { ProjectGroupResponse } from '@/api/client'
import { announceMutationFailure } from '@/lib/mutation-error-toast'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { DisconnectRepositoryDialog } from './DisconnectRepositoryDialog'

installBrowserWindow()
afterEach(() => {
  cleanup()
  spyOn(toast, 'error').mockRestore()
})

const group = {
  id: 1,
  name: 'Storefront',
  slug: 'storefront',
  projects: [],
  repository: { repo_owner: 'example-org', repo_name: 'example-repo' },
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
} as unknown as ProjectGroupResponse

function renderDialog() {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: {
        retry: false,
        // Same function the app-wide handler in App.tsx delegates to.
        onError: (error, _variables, _onMutateResult, context) =>
          announceMutationFailure(
            error as { title?: string; detail?: string },
            context
          ),
      },
    },
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <DisconnectRepositoryDialog
          group={group}
          open
          onOpenChange={() => {}}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('a failed disconnect is announced once, by the app-wide mutation handler', async () => {
  const error = spyOn(toast, 'error').mockImplementation(() => 'id')
  const api = mockApi({
    'DELETE /project-groups/1/repository': () =>
      jsonResponse(
        { title: 'Internal Server Error', detail: 'The repository is locked' },
        500
      ),
  })
  const view = renderDialog()

  fireEvent.click(await view.findByRole('button', { name: 'Disconnect' }))

  await waitFor(() => expect(error).toHaveBeenCalled())
  // Give a second, local toast the chance to fire before counting.
  await new Promise((resolve) => setTimeout(resolve, 50))
  expect(error).toHaveBeenCalledTimes(1)
  expect(error).toHaveBeenCalledWith('Could not disconnect the repository', {
    description: 'The repository is locked',
  })
  api.restore()
})
