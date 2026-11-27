// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { cleanup, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import {
  AuthContext,
  type AuthContextType,
} from '@/contexts/AuthContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { Projects } from './Projects'

installBrowserWindow()
afterEach(cleanup)

const day = (n: number) => Date.parse(`2026-11-${String(n).padStart(2, '0')}`)
const service = (id: number, name: string, at: number | null) => ({
  id,
  name,
  slug: name,
  source_type: 'static_files',
  last_deployment: at,
})
const member = (id: number, name: string, at: number | null) => ({
  ...service(id, name, null),
  last_deployment: at === null ? null : new Date(at).toISOString(),
})

// Two projects and eight services outside them: ten cards.
const GROUPS = [
  {
    id: 1,
    name: 'shop',
    slug: 'shop',
    projects: [member(100, 'storefront', day(26))],
  },
  {
    id: 2,
    name: 'observability',
    slug: 'observability',
    projects: [member(200, 'metrics-relay', null)],
  },
]
// In the server's order: newest first, never deployed last.
const SERVICES = [
  service(1, 'web-alpha', day(25)),
  service(2, 'landing-page', day(23)),
  service(3, 'notes-app', day(21)),
  service(4, 'status-page', day(19)),
  service(5, 'api-gateway', day(17)),
  service(6, 'search-indexer', day(7)),
  service(7, 'image-resizer', null),
  service(8, 'docs-site', null),
]

function renderProjects(path: string) {
  const api = mockApi({
    'GET /project-groups': () =>
      jsonResponse({ groups: GROUPS, total: GROUPS.length }),
    'GET /projects': (request) => {
      const query = new URL(request.url).searchParams
      const page = Number(query.get('page') ?? 1)
      const perPage = Number(query.get('per_page') ?? 20)
      return jsonResponse({
        projects: SERVICES.slice((page - 1) * perPage, page * perPage),
        total: SERVICES.length,
        page,
        per_page: perPage,
      })
    },
  })
  const auth = {
    user: { id: 1, role: 'admin' },
    isLoading: false,
    error: null,
    logout: async () => undefined,
    refetch: () => undefined,
  } as unknown as AuthContextType
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <AuthContext.Provider value={auth}>
        <MemoryRouter initialEntries={[path]}>
          <BreadcrumbProvider>
            <Routes>
              <Route path="/projects" element={<Projects />} />
            </Routes>
          </BreadcrumbProvider>
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
  return { view, api }
}

/** The cards on the page, by name, in order. */
const cardNames = () =>
  [...SERVICES, ...GROUPS]
    .map((item) => item.name)
    .filter((name) => document.body.textContent?.includes(name))
    .sort(
      (a, b) =>
        (document.body.textContent ?? '').indexOf(a) -
        (document.body.textContent ?? '').indexOf(b)
    )

describe('the Projects page pages projects and services together', () => {
  test('a page of nine holds nine cards, projects included', async () => {
    const { api } = renderProjects('/projects?page=1&page_size=9')
    await waitFor(() => expect(cardNames()).toContain('web-alpha'))
    expect(cardNames()).toEqual([
      'shop',
      'web-alpha',
      'landing-page',
      'notes-app',
      'status-page',
      'api-gateway',
      'search-indexer',
      'observability',
      'image-resizer',
    ])
    expect(document.body.textContent).not.toContain('docs-site')
    api.restore()
  })

  test('the card a page leaves out opens the next one', async () => {
    const { api } = renderProjects('/projects?page=2&page_size=9')
    await waitFor(() => expect(cardNames()).toContain('docs-site'))
    expect(cardNames()).toEqual(['docs-site'])
    api.restore()
  })
})
