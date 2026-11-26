// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { ProjectGroupResponse } from '@/api/client/types.gen'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { Globe } from 'lucide-react'
import { installBrowserWindow } from '@/test/dom'
import { GROUP_FANOUT_LIMIT } from '@/lib/group-scoped-lists'
import {
  GroupDeploymentsPage,
  GroupDomainsPage,
  ScopedList,
} from './GroupScopedPages'

installBrowserWindow()
afterEach(cleanup)

const complete = { loading: false, failed: 0, truncated: false, refetch() {} }

function renderList(state: typeof complete, isEmpty: boolean) {
  render(
    <ScopedList
      state={state}
      isEmpty={isEmpty}
      noun="domains"
      icon={Globe}
      emptyTitle="No custom domains in this project yet"
      emptyDescription="Domains are added to a service."
      emptyAction={<button type="button">Add domain</button>}
    >
      <p>the list</p>
    </ScopedList>
  )
}

describe('a scoped list', () => {
  test('is empty only when every request succeeded', () => {
    renderList(complete, true)
    expect(screen.getByText('No custom domains in this project yet'))
    expect(screen.getByRole('button', { name: 'Add domain' }))
  })

  test('a failed read is a failure with a retry, not an empty project', () => {
    renderList({ ...complete, failed: 2 }, true)
    expect(
      screen.queryByText('No custom domains in this project yet')
    ).toBeNull()
    expect(screen.queryByRole('button', { name: 'Add domain' })).toBeNull()
    expect(screen.getByText("This project's domains could not be read in full"))
    expect(screen.getByText("2 requests for this project's domains failed."))
    expect(screen.getByRole('button', { name: 'Retry' }))
  })

  test('an empty capped read says what was left out', () => {
    renderList({ ...complete, truncated: true }, true)
    expect(
      screen.queryByText('No custom domains in this project yet')
    ).toBeNull()
    expect(
      screen.getByText(
        `Only the first ${GROUP_FANOUT_LIMIT} services are included, and they have no domains.`
      )
    )
  })

  test('shows a skeleton only while there is nothing to show', () => {
    renderList({ ...complete, loading: true }, true)
    expect(screen.getByRole('status', { name: 'Loading domains' }))
    cleanup()
    renderList({ ...complete, loading: true }, false)
    expect(screen.getByText('the list'))
    expect(screen.queryByRole('status', { name: 'Loading domains' })).toBeNull()
  })

  test('a partial list shows what it has and says it is partial', () => {
    renderList({ ...complete, truncated: true }, false)
    expect(screen.getByText('the list'))
    expect(screen.getByText('This list may be incomplete'))
  })
})

describe('a project page covers only the services it applies to', () => {
  const app = {
    id: 11,
    name: 'web',
    slug: 'web',
    hosting: 'hosted',
    source_type: 'git',
    project_type: 'server',
  }
  const monitored = {
    id: 12,
    name: 'legacy',
    slug: 'legacy',
    hosting: 'external',
    source_type: 'external',
    project_type: 'server',
  }
  const project = (...projects: object[]) =>
    ({
      id: 3,
      name: 'Shop',
      slug: 'shop',
      projects,
    }) as unknown as ProjectGroupResponse

  function renderDomains(group: ProjectGroupResponse, domains: object[] = []) {
    const mock = mockApi({
      'GET /projects/11/custom-domains': () =>
        jsonResponse({ domains, total: domains.length }),
    })
    render(
      <QueryClientProvider
        client={
          new QueryClient({ defaultOptions: { queries: { retry: false } } })
        }
      >
        <MemoryRouter>
          <BreadcrumbProvider>
            <GroupDomainsPage group={group} />
          </BreadcrumbProvider>
        </MemoryRouter>
      </QueryClientProvider>
    )
    return mock
  }

  test('domains are read only from services that can have one', async () => {
    const mock = renderDomains(project(app, monitored))
    expect(await screen.findByText('No custom domains in this project yet'))
    const paths = mock.calls.map((call) => `${call.method} ${call.path}`)
    expect(paths).toContain('GET /projects/11/custom-domains')
    expect(paths).not.toContain('GET /projects/12/custom-domains')
    mock.restore()
  })

  test("a domain names its service with the service's icon", async () => {
    const mock = renderDomains(project(app), [
      { id: 5, domain: 'shop.example.com', status: 'active' },
    ])
    await screen.findByText('shop.example.com')
    const service = screen.getByRole('link', { name: 'web' })
    expect(service.getAttribute('href')).toBe('/projects/web')
    expect(service.querySelector('span[aria-hidden="true"]')).toBeTruthy()
    mock.restore()
  })

  test('a project of monitored services only says why it has no domains', () => {
    const mock = renderDomains(project(monitored))
    expect(
      screen.getByText(
        "None of this project's services take a custom domain here"
      )
    )
    expect(
      screen.getByRole('link', { name: 'Add service' }).getAttribute('href')
    ).toContain('3')
    expect(mock.calls).toHaveLength(0)
    mock.restore()
  })

  test('a deployment links to its service and to itself', async () => {
    const mock = mockApi({
      'GET /projects/11/deployments': () =>
        jsonResponse({
          deployments: [
            {
              id: 6,
              project_id: 11,
              status: 'completed',
              created_at: 1_767_225_600_000,
              environment: { id: 1, name: 'production' },
              environment_id: 1,
            },
          ],
          total: 1,
          page: 1,
          per_page: 5,
        }),
    })
    render(
      <QueryClientProvider
        client={
          new QueryClient({ defaultOptions: { queries: { retry: false } } })
        }
      >
        <MemoryRouter>
          <BreadcrumbProvider>
            <GroupDeploymentsPage group={project(app)} />
          </BreadcrumbProvider>
        </MemoryRouter>
      </QueryClientProvider>
    )
    const deployment = await screen.findByRole('link', {
      name: 'Deployment #6 of web',
    })
    expect(deployment.getAttribute('href')).toBe('/projects/web/deployments/6')
    expect(screen.getByRole('link', { name: 'web' }).getAttribute('href')).toBe(
      '/projects/web'
    )
    mock.restore()
  })
})
