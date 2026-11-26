// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import {
  QueryClient,
  QueryClientProvider,
  useQuery,
} from '@tanstack/react-query'
import { wireguardMeshStatusGetOptions } from '@/api/client/@tanstack/react-query.gen'
import type { WireguardMeshStatusResponse } from '@/api/client/types.gen'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { MeshTurnOffCard } from './MeshTurnOffCard'

afterEach(cleanup)

function status(
  overrides: Partial<WireguardMeshStatusResponse>
): WireguardMeshStatusResponse {
  return {
    state: 'ready',
    reason: null,
    cidr: '10.201.0.0/24',
    pool_cidr: '10.201.0.0/24',
    ca_rotation_required: false,
    listen_port: 51820,
    control_plane: null,
    can_enable: false,
    enable_blocker: null,
    can_disable: true,
    disable_blocker: null,
    enable_command: 'temps network setup-multi-node --wireguard',
    join_url: null,
    handshake_error: null,
    nodes: [],
    hub: null,
    links: [],
    ...overrides,
  }
}

function Card() {
  const { data } = useQuery(wireguardMeshStatusGetOptions())
  return <MeshTurnOffCard mesh={data} />
}

test('a status read in flight when the mesh is turned off does not bring it back', async () => {
  let releaseStaleRead: () => void = () => {}
  const staleRead = new Promise<void>((resolve) => {
    releaseStaleRead = resolve
  })
  mockApi({
    'GET /nodes/wireguard': async () => {
      await staleRead
      return jsonResponse(status({ state: 'ready', can_disable: true }))
    },
    'DELETE /nodes/wireguard': () =>
      jsonResponse(
        status({ state: 'disabled', cidr: null, can_disable: false })
      ),
  })
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: Infinity },
      mutations: { retry: false },
    },
  })
  const key = wireguardMeshStatusGetOptions().queryKey
  client.setQueryData(key, status({ state: 'ready', can_disable: true }))
  const view = render(
    <QueryClientProvider client={client}>
      <Card />
    </QueryClientProvider>
  )

  // A poll that started before the turn-off and answers after it.
  void client.refetchQueries({ queryKey: key })
  await waitFor(() => expect(client.isFetching({ queryKey: key })).toBe(1))

  fireEvent.click(view.getByRole('button', { name: 'Turn off mesh' }))
  const turnOff = await view.findAllByRole('button', { name: 'Turn off mesh' })
  fireEvent.click(turnOff[turnOff.length - 1])
  await waitFor(() =>
    expect(view.getByText('The WireGuard mesh is off.')).toBeTruthy()
  )

  releaseStaleRead()
  await new Promise((resolve) => setTimeout(resolve, 20))
  expect(client.getQueryData<WireguardMeshStatusResponse>(key)?.state).toBe(
    'disabled'
  )
  expect(view.getByText('The WireGuard mesh is off.')).toBeTruthy()
})

test('a mesh in use shows why it cannot be turned off', () => {
  const view = render(
    <QueryClientProvider client={new QueryClient()}>
      <MeshTurnOffCard
        mesh={status({
          can_disable: false,
          disable_blocker: '1 node is on the mesh (worker-1).',
        })}
      />
    </QueryClientProvider>
  )
  expect(view.getByText('1 node is on the mesh (worker-1).')).toBeTruthy()
  expect(
    (
      view.getByRole('button', {
        name: 'Turn off mesh',
      }) as HTMLButtonElement
    ).disabled
  ).toBe(true)
})
