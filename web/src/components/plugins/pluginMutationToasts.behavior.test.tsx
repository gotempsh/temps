// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, spyOn, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  act,
  cleanup,
  fireEvent,
  render,
  renderHook,
  waitFor,
} from '@testing-library/react'
import type { ReactNode } from 'react'
import { toast } from 'sonner'
import { useReloadPlugins, useUninstallPlugin } from '@/hooks/usePlugins'
import { announceMutationFailure } from '@/lib/mutation-error-toast'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { RepositoryUpdate } from './RepositoryUpdate'

// The static check in pluginMutationToasts.test.ts only proves the catch
// blocks stay silent. These run a rejected plugin mutation under the same
// handler App.tsx installs and count what the user would actually see, so a
// hook-level `onError` that swallows the shared toast, or a second toast
// raised anywhere else, fails here.

installBrowserWindow()
afterEach(() => {
  cleanup()
  spyOn(toast, 'error').mockRestore()
})

const FAILURE = {
  title: 'Bad Gateway',
  detail: 'The plugin host refused the request',
}

function appClient() {
  return new QueryClient({
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
}

function wrapper(client: QueryClient) {
  return function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>
  }
}

// Long enough for a second, local toast to fire before the count is taken.
const settle = () => new Promise((resolve) => setTimeout(resolve, 50))

function expectOneToast(error: ReturnType<typeof spyOn>) {
  expect(error).toHaveBeenCalledTimes(1)
  expect(error).toHaveBeenCalledWith(FAILURE.title, {
    description: FAILURE.detail,
  })
}

test('a failed repository update shows exactly one toast with the title and detail', async () => {
  const error = spyOn(toast, 'error').mockImplementation(() => 'id')
  const api = mockApi({
    'GET /x/plugins/demo/status': () =>
      jsonResponse({
        configured: true,
        source: {
          kind: 'github',
          repository_url: 'https://github.com/example/plugins',
          path: 'plugins/demo',
          ref_name: 'main',
          commit: 'a'.repeat(40),
          version: '1.0.0',
          builder_image: 'fixture',
        },
      }),
    'POST /x/plugins/demo/update': () => jsonResponse(FAILURE, 502),
  })
  const view = render(
    <QueryClientProvider client={appClient()}>
      <RepositoryUpdate
        name="demo"
        disabled={false}
        onSensitiveError={() => false}
      />
    </QueryClientProvider>
  )

  fireEvent.click(
    await view.findByRole('button', { name: 'Update from GitHub' })
  )

  await waitFor(() => expect(error).toHaveBeenCalled())
  await settle()
  expectOneToast(error)
  api.restore()
})

test('a failed uninstall shows exactly one toast with the title and detail', async () => {
  const error = spyOn(toast, 'error').mockImplementation(() => 'id')
  const api = mockApi({
    'POST /x/plugins/demo/uninstall': () => jsonResponse(FAILURE, 502),
  })
  const { result } = renderHook(() => useUninstallPlugin(), {
    wrapper: wrapper(appClient()),
  })

  await act(async () => {
    await result.current.mutateAsync('demo').catch(() => {})
  })

  await settle()
  expectOneToast(error)
  api.restore()
})

test('a failed reload shows exactly one toast with the title and detail', async () => {
  const error = spyOn(toast, 'error').mockImplementation(() => 'id')
  const api = mockApi({
    'POST /x/plugins/reload': () => jsonResponse(FAILURE, 502),
  })
  const { result } = renderHook(() => useReloadPlugins(), {
    wrapper: wrapper(appClient()),
  })

  await act(async () => {
    await result.current.mutateAsync().catch(() => {})
  })

  await settle()
  expectOneToast(error)
  api.restore()
})
