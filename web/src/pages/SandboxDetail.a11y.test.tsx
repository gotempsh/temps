// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { act, cleanup, render } from '@testing-library/react'
import axe from 'axe-core'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { getSandboxOptions } from '@/api/client/@tanstack/react-query.gen'
import type { SandboxResponse } from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { installBrowserWindow } from '@/test/dom'
import SandboxDetail from './SandboxDetail'

installBrowserWindow()
afterEach(cleanup)

const sandbox = {
  sandbox: {
    allow_docker: false,
    backend: 'docker',
    createdAt: 1_767_225_600_000,
    cwd: '/workspace',
    id: 'sbx-1',
    image: 'example/runtime:latest',
    lifecycle: 'ephemeral',
    memory: 2048,
    name: 'example-sandbox',
    node_name: 'control-plane',
    preview_url_template: 'https://{port}-example.test',
    preview_access: 'temps_login',
    preview_allowed_cidrs: [],
    egress_policy: { mode: 'internet', rules: [] },
    region: 'local',
    requestedAt: 1_767_225_600_000,
    runtime: 'node22',
    status: 'running',
    timeout: 600_000,
    updatedAt: 1_767_225_660_000,
    vcpus: 2,
  },
  routes: [],
} as unknown as SandboxResponse

test('the command runner fields are named, not just titled', async () => {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  client.setQueryData(
    getSandboxOptions({ path: { id: 'sbx-1' } }).queryKey,
    sandbox
  )
  const view = render(
    <QueryClientProvider client={client}>
      <AuthContext.Provider
        value={{
          user: null,
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <MemoryRouter initialEntries={['/sandboxes/sbx-1']}>
          <Routes>
            <Route path="/sandboxes/:sandboxId" element={<SandboxDetail />} />
          </Routes>
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 20))
  })

  const cwd = view.getByRole('textbox', {
    name: 'Working directory (optional)',
  })
  expect(cwd.getAttribute('placeholder')).toBe('/workspace')
  view.getByRole('textbox', { name: 'Command to run in the sandbox' })

  const result = await axe.run(view.container, {
    runOnly: { type: 'rule', values: ['label', 'label-title-only'] },
  })
  expect(result.violations.map((v) => `${v.id}: ${v.nodes.length}`)).toEqual([])
  client.clear()
})
