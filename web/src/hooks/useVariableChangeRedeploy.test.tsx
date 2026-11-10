// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, spyOn, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, renderHook } from '@testing-library/react'
import type { ReactNode } from 'react'
import { toast } from 'sonner'
import type { SourceType } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { useVariableChangeRedeploy } from './useVariableChangeRedeploy'

installBrowserWindow()
afterEach(cleanup)

async function confirmChange(
  sourceType: SourceType,
  deployment: Record<string, unknown> = {}
) {
  const api = mockApi({
    'GET /projects/7/environments': () =>
      jsonResponse([
        {
          id: 3,
          slug: 'production',
          name: 'production',
          is_preview: false,
          current_deployment_id: 31,
        },
      ]),
    'GET /projects/7/deployments/31': () =>
      jsonResponse({ id: 31, ...deployment }),
  })
  const success = spyOn(toast, 'success').mockImplementation(() => 1)
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  )
  const { result } = renderHook(
    () => useVariableChangeRedeploy({ id: 7, source_type: sourceType }),
    { wrapper }
  )
  await result.current('API_KEY updated', 'all')
  const call = success.mock.calls[0] as [string, Record<string, unknown>]
  success.mockRestore()
  api.restore()
  return call
}

test('a Git project is offered a Redeploy action after a variable changes', async () => {
  const [title, options] = await confirmChange('git', {
    branch: 'main',
    commit_hash: 'abc123',
  })
  expect(title).toBe('API_KEY updated')
  expect(options.action).toMatchObject({ label: 'Redeploy' })
  expect(options.description).toContain('Redeploy production')
})

test('an uploaded source is told to upload again, with no Redeploy action to be declined', async () => {
  const [, options] = await confirmChange('uploaded_source')
  expect(options.action).toBeUndefined()
  expect(options.description).toBe(
    'Applies on next deploy. Upload the source again to apply it now.'
  )
})

test('a manual project running an image keeps its Redeploy action', async () => {
  const [, options] = await confirmChange('manual', {
    metadata: { externalImageRef: 'app:1' },
  })
  expect(options.action).toMatchObject({ label: 'Redeploy' })
})

test('a Git project whose live deployment is an uploaded archive is told to upload again', async () => {
  const [, options] = await confirmChange('git', {
    metadata: { deploymentSourceType: 'uploaded_source' },
  })
  expect(options.action).toBeUndefined()
  expect(options.description).toContain('Upload the source again')
})
