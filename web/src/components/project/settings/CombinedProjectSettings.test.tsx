// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { AuthProvider } from '@/contexts/AuthContext'
import { PresetProvider } from '@/contexts/PresetContext'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { CombinedProjectSettings } from './CombinedProjectSettings'

test('intro cards keep their padding outside the settings section spacing rules', () => {
  const client = new QueryClient()
  const html = renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <AuthProvider>
          <PresetProvider>
            <CombinedProjectSettings
              page="delivery"
              project={
                {
                  id: 1,
                  slug: 'example',
                  hosting: 'external',
                } as ProjectResponse
              }
              refetch={() => {}}
              intro={
                <section data-testid="runtime-summary" className="px-4 py-3">
                  Runtime
                </section>
              }
            />
          </PresetProvider>
        </AuthProvider>
      </QueryClientProvider>
    </MemoryRouter>
  )
  const root = document.createElement('div')
  root.innerHTML = html
  const summary = root.querySelector('[data-testid="runtime-summary"]')!
  expect(summary).not.toBeNull()
  expect(summary.parentElement?.className).not.toContain('[&>section]')
  expect(summary.parentElement?.className).not.toContain(
    '[&>section:first-child]'
  )
  expect(summary.classList.contains('py-3')).toBe(true)
  client.clear()
})
