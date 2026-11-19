// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectResponse } from '@/api/client'
import { getProjectQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import { GeneratedProjectCollection } from './GeneratedProjectCollection'

function render(project: Partial<ProjectResponse> | null) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  if (project) {
    client.setQueryData(
      getProjectQueryKey({ path: { id: 2 } }),
      project as ProjectResponse
    )
  }
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <GeneratedProjectCollection
          presentation={{
            total: 1,
            page: 1,
            perPage: 20,
            items: [
              {
                id: 2,
                name: 'example-web',
                slug: 'example-web',
                repoOwner: 'acme',
                repoName: 'examples',
                preset: 'nextjs',
              },
            ],
          }}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

describe('GeneratedProjectCollection labels', () => {
  test('calls projects projects, so Vault services are not confused with them', () => {
    const markup = render({
      id: 2,
      name: 'example-web',
      slug: 'example-web',
      preset: 'nextjs',
      source_type: 'git',
    })
    expect(markup).toContain('Projects')
    expect(markup).toContain('1 project')
    expect(markup).not.toMatch(/service/i)
  })
})

describe('GeneratedProjectCollection icons', () => {
  test('shows the preset logo and the project initial, not two letters of the name', () => {
    const markup = render({
      id: 2,
      name: 'example-web',
      slug: 'example-web',
      preset: 'nextjs',
      source_type: 'git',
      git_url: 'https://github.com/acme/examples',
      repo_owner: 'acme',
      repo_name: 'examples',
    })
    expect(markup).toMatch(/>E</)
    expect(markup).toContain('Next.js logo')
    expect(markup).not.toMatch(/>ex</)
  })

  test('still shows the preset logo while the project is not loaded', () => {
    const markup = render(null)
    expect(markup).toContain('Next.js logo')
    expect(markup).toMatch(/>E</)
  })
})
