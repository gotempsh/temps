// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import {
  getEnvironmentIpAccessQueryKey,
  getProjectIpAccessQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ProjectIpAccessDetail,
  UptimeMonitorIpBlock,
} from '@/api/client/types.gen'
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
  projectSlug,
}: {
  environmentId?: number
  data?: ProjectIpAccessDetail
  failed?: boolean
  projectSlug?: string
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
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <ProjectIpAccessCard
          projectId={2}
          projectSlug={projectSlug}
          environmentId={environmentId}
        />
      </QueryClientProvider>
    </MemoryRouter>
  )
  client.clear()
  return markup
}

test('shows a skeleton while the list loads', () => {
  const markup = render({})
  expect(markup).toContain('aria-busy="true"')
  expect(markup).not.toContain('Save IP access list')
})

test('an unconfigured project explains the modes, with nothing to edit while Off', () => {
  const markup = render({ data: unconfigured })
  expect(markup).toContain('Open to everyone')
  expect(markup).toContain('Only allow listed addresses')
  expect(markup).toContain('Block listed addresses')
  // Off lets everyone through: no list, presets or address check until a
  // list mode is chosen.
  expect(markup).not.toContain('Addresses and ranges')
  expect(markup).not.toContain('Add Tailscale ranges')
  expect(markup).not.toContain('Add my IP')
  expect(markup).not.toContain('Try an address')
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
  expect(markup).toContain('Follows the project list')
  expect(markup).toContain('Follow the project list')
  // Off must be reachable as an explicit override of the project list.
  expect(markup).toContain('Off')
  expect(markup).not.toContain('Use project list')
  // Nothing to edit until the environment picks its own mode.
  expect(markup).not.toContain('Add range')
})

test('a load failure is reported with a retry', () => {
  const markup = render({ failed: true })
  expect(markup).toContain('Could not load the IP access list')
  expect(markup).toContain('Try again')
})

const monitorBlock: UptimeMonitorIpBlock = {
  project_id: 2,
  environment_id: null,
  policy_scope: 'project',
  mode: 'allow',
  reason: 'not_allow_listed',
  monitor_addresses: ['127.0.0.1', '203.0.113.10'],
  blocked_addresses: ['203.0.113.10'],
  matched_cidrs: [],
  suggested_cidrs: ['203.0.113.10/32'],
  suggested_description: 'Temps uptime monitor',
  message:
    "Blocked by this project's IP access list: Temps' uptime monitor connects from 203.0.113.10, which the list does not allow.",
}

const allowList: ProjectIpAccessDetail = {
  ...unconfigured,
  configured: true,
  mode: 'allow',
  cidrs: [{ cidr: '198.51.100.7/32' }, { cidr: '100.64.0.0/10' }],
  updated_at: '2026-10-13T09:30:00Z',
}

test('a list that refuses the uptime monitor says so and offers the fix', () => {
  const markup = render({
    data: { ...allowList, uptime_monitor_block: monitorBlock },
  })
  expect(markup).toContain('This list blocks the uptime monitor')
  expect(markup).toContain('connects from 203.0.113.10')
  expect(markup).toContain('Allow monitor')
  expect(markup).toContain('Add 203.0.113.10/32 as')
})

test('a full list says what to remove before the monitor fits', () => {
  const full = Array.from({ length: 256 }, (_, i) => ({
    cidr: `10.0.${i}.0/24`,
  }))
  const markup = render({
    data: { ...allowList, cidrs: full, uptime_monitor_block: monitorBlock },
  })
  expect(markup).toContain(
    'The list is full. Remove 1 entry to make room for the monitor.'
  )
  expect(markup).toMatch(/<button[^>]*disabled=""[^>]*>Allow monitor/)
})

test('a list that lets the monitor in shows no monitor warning', () => {
  const markup = render({ data: allowList })
  expect(markup).not.toContain('blocks the uptime monitor')
  expect(markup).not.toContain('Allow monitor')
})

test('an environment following a refusing project list links to that list', () => {
  const markup = render({
    environmentId: 7,
    projectSlug: 'web',
    data: {
      ...unconfigured,
      environment_id: 7,
      uptime_monitor_block: { ...monitorBlock, environment_id: 7 },
    },
  })
  expect(markup).toContain('This list blocks the uptime monitor')
  // The fix belongs on the project list, not on this environment's.
  expect(markup).not.toContain('Allow monitor')
  expect(markup).toContain('href="/projects/web/settings/security#ip-access"')
  expect(markup).toContain('Project list')
})

test('a deny list covering the monitor explains it without an add button', () => {
  const markup = render({
    data: {
      ...allowList,
      mode: 'deny',
      uptime_monitor_block: {
        ...monitorBlock,
        mode: 'deny',
        reason: 'deny_listed',
        matched_cidrs: ['127.0.0.0/8'],
        suggested_cidrs: [],
        message:
          "Blocked by this project's IP access list: remove 127.0.0.0/8.",
      },
    },
  })
  expect(markup).toContain('remove 127.0.0.0/8')
  expect(markup).not.toContain('Allow monitor')
})
