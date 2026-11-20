// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import { AuthContext } from '@/contexts/AuthContext-shared'
import { SandboxSettingsPage } from './SandboxSettingsPage'

const auth = {
  user: { id: 1, role: 'admin' },
  isLoading: false,
  error: null,
  logout: async () => {},
  refetch: () => {},
} as unknown as React.ContextType<typeof AuthContext>

function render(path: string) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <AuthContext.Provider value={auth}>
        <MemoryRouter initialEntries={[path]}>
          <SandboxSettingsPage />
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

const sectionTag = (html: string, id: string) =>
  html.match(new RegExp(`<div data-section="${id}"[^>]*>`))?.[0] ?? ''

/**
 * Each section keeps unsaved edits in its own state, so switching tabs must
 * hide the other section rather than unmount it.
 */
test('both sections stay mounted and only the current one is shown', () => {
  const runtime = render('/sandboxes/settings')
  expect(sectionTag(runtime, 'runtime')).not.toContain('hidden')
  expect(sectionTag(runtime, 'preview')).toContain('hidden')

  const preview = render('/sandboxes/settings/preview')
  expect(sectionTag(preview, 'runtime')).toContain('hidden')
  expect(sectionTag(preview, 'preview')).not.toBe('')
  expect(sectionTag(preview, 'preview')).not.toContain('hidden')
})
