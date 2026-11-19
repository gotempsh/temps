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
import { Toaster, toast } from 'sonner'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { PurgeAssetCacheCard } from './PurgeAssetCacheCard'

installBrowserWindow()
afterEach(() => {
  cleanup()
  toast.dismiss()
})

const PURGE = 'DELETE /projects/3/environments/8/asset-cache'

function renderCard() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <PurgeAssetCacheCard
        projectId={3}
        environmentId={8}
        environmentName="production"
      />
      <Toaster />
    </QueryClientProvider>
  )
}

async function purge(view: ReturnType<typeof render>) {
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Purge asset cache' }))
  })
  await act(async () => {
    fireEvent.click(await view.findByRole('button', { name: 'Purge cache' }))
  })
}

test('a refused purge is reported with the reason, not as "Purged 0"', async () => {
  const api = mockApi({
    [PURGE]: () =>
      jsonResponse(
        {
          title: 'Forbidden',
          status: 403,
          detail: 'Requires deployments:write permission',
        },
        403
      ),
  })
  const view = renderCard()
  await purge(view)

  await waitFor(() =>
    expect(api.called('DELETE', '/projects/3/environments/8/asset-cache')).toBe(
      true
    )
  )
  expect(await view.findByText('Failed to purge asset cache')).toBeTruthy()
  expect(
    await view.findByText('Requires deployments:write permission')
  ).toBeTruthy()
  expect(view.queryByText(/^Purged \d+ cached asset/)).toBeNull()
  // The confirmation stays open for a retry.
  expect(
    view.getByText('Purge asset cache of environment production?')
  ).toBeTruthy()
  api.restore()
})

test('an accepted purge reports how many assets were removed', async () => {
  const api = mockApi({ [PURGE]: () => jsonResponse({ deleted: 12 }) })
  const view = renderCard()
  await purge(view)

  expect(await view.findByText('Purged 12 cached assets')).toBeTruthy()
  await waitFor(() =>
    expect(
      view.queryByText('Purge asset cache of environment production?')
    ).toBeNull()
  )
  api.restore()
})
