// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import {
  getEnvironmentIpAccessQueryKey,
  getProjectIpAccessQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type { ProjectIpAccessDetail } from '@/api/client/types.gen'
import { ProjectIpAccessCard } from './ProjectIpAccessCard'

const unconfigured: ProjectIpAccessDetail = {
  project_id: 2,
  environment_id: null,
  configured: false,
  mode: 'disabled',
  cidrs: [],
  max_cidrs: 256,
  updated_at: null,
  your_ip: '203.0.113.9',
}

function render({
  environmentId,
  data,
  failed = false,
}: {
  environmentId?: number
  data?: ProjectIpAccessDetail
  failed?: boolean
}) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  const queryKey =
    environmentId === undefined
      ? getProjectIpAccessQueryKey({ path: { project_id: 2 } })
      : getEnvironmentIpAccessQueryKey({
          path: { project_id: 2, environment_id: environmentId },
        })
  if (data) client.setQueryData(queryKey, data)
  if (failed) {
    client
      .getQueryCache()
      .build(client, { queryKey })
      .setState({
        status: 'error',
        error: new Error('Offline'),
        fetchStatus: 'idle',
      })
  }
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <ProjectIpAccessCard projectId={2} environmentId={environmentId} />
    </QueryClientProvider>
  )
  client.clear()
  return markup
}

test('shows a skeleton while the list loads', () => {
  const markup = render({})
  expect(markup).toContain('aria-busy="true"')
  expect(markup).not.toContain('Save IP access list')
})

test('an unconfigured project explains the feature and offers presets', () => {
  const markup = render({ data: unconfigured })
  expect(markup).toContain('Open to everyone')
  expect(markup).toContain('Only allow listed addresses')
  expect(markup).toContain('Block listed addresses')
  expect(markup).toContain('Add Tailscale ranges')
  expect(markup).toContain('Add my IP (203.0.113.9)')
  expect(markup).not.toContain('Remove list')
})

test('a saved allow list is summarised and can be removed', () => {
  const markup = render({
    data: {
      ...unconfigured,
      configured: true,
      mode: 'allow',
      cidrs: [{ cidr: '100.64.0.0/10' }, { cidr: 'fd7a:115c:a1e0::/48' }],
      updated_at: '2026-10-13T09:30:00Z',
    },
  })
  expect(markup).toContain('Only 2 ranges can reach this')
  expect(markup).toContain('Remove list')
})

test('an environment without its own list says it follows the project', () => {
  const markup = render({
    environmentId: 7,
    data: { ...unconfigured, environment_id: 7 },
  })
  expect(markup).toContain('follows the project list')
  expect(markup).not.toContain('Use project list')
})

test('a load failure is reported with a retry', () => {
  const markup = render({ failed: true })
  expect(markup).toContain('Could not load the IP access list')
  expect(markup).toContain('Try Again')
})
