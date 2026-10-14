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
import {
  getEnvironmentIpAccessQueryKey,
  getProjectIpAccessQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type { ProjectIpAccessDetail } from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ProjectIpAccessCard } from './ProjectIpAccessCard'

installBrowserWindow()
afterEach(cleanup)

const base: ProjectIpAccessDetail = {
  project_id: 2,
  environment_id: null,
  configured: true,
  mode: 'allow',
  cidrs: [{ cidr: '10.0.0.0/8' }],
  max_cidrs: 256,
  updated_at: '2026-10-13T09:30:00Z',
  your_ip: '203.0.113.9',
}

function mount({
  data,
  environmentId,
}: {
  data: ProjectIpAccessDetail
  environmentId?: number
}) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  const queryKey =
    environmentId === undefined
      ? getProjectIpAccessQueryKey({ path: { project_id: 2 } })
      : getEnvironmentIpAccessQueryKey({
          path: { project_id: 2, environment_id: environmentId },
        })
  client.setQueryData(queryKey, data)
  const view = render(
    <QueryClientProvider client={client}>
      <ProjectIpAccessCard projectId={2} environmentId={environmentId} />
    </QueryClientProvider>
  )
  return { client, queryKey, view }
}

function filler(count: number) {
  return Array.from({ length: count }, (_, index) => ({
    cidr: `10.${Math.floor(index / 256)}.${index % 256}.0/24`,
  }))
}

test('an environment set to follow the project is tested against the project list', async () => {
  const api = mockApi({
    'POST /projects/2/ip-access/test': () =>
      jsonResponse({
        ip: '198.51.100.7',
        allowed: false,
        reason: 'not in the allow list',
        mode: 'allow',
        matched_cidr: null,
      }),
  })
  const { view } = mount({
    environmentId: 7,
    data: { ...base, environment_id: 7 },
  })

  // The environment holds an override, but the operator picks "follow".
  fireEvent.click(
    view.getByRole('radio', { name: /Follow the project list/ })
  )
  fireEvent.change(view.getByLabelText('Try an address'), {
    target: { value: '198.51.100.7' },
  })
  fireEvent.click(view.getByRole('button', { name: 'Check' }))

  expect(await view.findByText(/Blocked/)).toBeTruthy()
  expect(api.called('POST', '/projects/2/ip-access/test')).toBe(true)
  expect(
    api.called('POST', '/projects/2/environments/7/ip-access/test')
  ).toBe(false)
  api.restore()
})

test('an environment with its own mode is still tested against itself', async () => {
  const api = mockApi({
    'POST /projects/2/environments/7/ip-access/test': () =>
      jsonResponse({
        ip: '10.1.1.1',
        allowed: true,
        reason: 'matches 10.0.0.0/8',
        mode: 'allow',
        matched_cidr: '10.0.0.0/8',
      }),
  })
  const { view } = mount({
    environmentId: 7,
    data: { ...base, environment_id: 7 },
  })
  fireEvent.change(view.getByLabelText('Try an address'), {
    target: { value: '10.1.1.1' },
  })
  fireEvent.click(view.getByRole('button', { name: 'Check' }))

  expect(await view.findByText(/Allowed/)).toBeTruthy()
  expect(api.called('POST', '/projects/2/ip-access/test')).toBe(false)
  api.restore()
})

test('a background change to the saved list does not erase unsaved edits', async () => {
  const { client, queryKey, view } = mount({ data: base })

  fireEvent.click(view.getByRole('button', { name: /Add range/ }))
  fireEvent.change(view.getByLabelText('Address or range 2'), {
    target: { value: '192.168.0.0/16' },
  })

  await act(async () => {
    client.setQueryData(queryKey, {
      ...base,
      cidrs: [{ cidr: '172.16.0.0/12' }],
      updated_at: '2026-10-13T10:00:00Z',
    })
  })

  // Query notifications are delivered on a timer, so wait for the card.
  expect(await view.findByText('The saved list changed')).toBeTruthy()
  const kept = view.getByLabelText('Address or range 2') as HTMLInputElement
  expect(kept.value).toBe('192.168.0.0/16')

  // Discarding is the explicit way to pick up the newer saved list.
  fireEvent.click(view.getByRole('button', { name: 'Discard changes' }))
  const first = view.getByLabelText('Address or range 1') as HTMLInputElement
  expect(first.value).toBe('172.16.0.0/12')
  expect(view.queryByLabelText('Address or range 2')).toBeNull()
  expect(view.queryByText('The saved list changed')).toBeNull()
})

test('a background change is loaded when there is nothing unsaved', async () => {
  const { client, queryKey, view } = mount({ data: base })

  await act(async () => {
    client.setQueryData(queryKey, {
      ...base,
      cidrs: [{ cidr: '172.16.0.0/12' }],
      updated_at: '2026-10-13T10:00:00Z',
    })
  })

  await waitFor(() => {
    const first = view.getByLabelText('Address or range 1') as HTMLInputElement
    expect(first.value).toBe('172.16.0.0/12')
  })
  expect(view.queryByText('The saved list changed')).toBeNull()
})

test('the Tailscale preset is unavailable when both ranges would not fit', () => {
  const { view } = mount({
    data: { ...base, cidrs: filler(255) },
  })
  const button = view.getByRole('button', {
    name: /Add Tailscale ranges/,
  }) as HTMLButtonElement
  expect(button.disabled).toBe(true)
  expect(button.title).toContain('Not enough room')
})

test('the Tailscale preset is available when both ranges fit', () => {
  const { view } = mount({
    data: { ...base, cidrs: filler(254) },
  })
  const button = view.getByRole('button', {
    name: /Add Tailscale ranges/,
  }) as HTMLButtonElement
  expect(button.disabled).toBe(false)
  fireEvent.click(button)
  expect(view.getByLabelText('Address or range 256')).toBeTruthy()
  expect(view.queryByLabelText('Address or range 257')).toBeNull()
})
