// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import type { ContainerSecurityResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ContainerSecuritySettings } from './ContainerSecuritySettings'

installBrowserWindow()
afterEach(cleanup)

const PATH = '/projects/9/container-security'
const LABEL = 'Allow binding ports below 1024 (NET_BIND_SERVICE)'

function security(
  overrides: Partial<ContainerSecurityResponse> = {}
): ContainerSecurityResponse {
  return {
    allowed_capabilities: [],
    options: [
      {
        id: 'NET_BIND_SERVICE',
        label: LABEL,
        description: 'Lets a setcap entrypoint start.',
        trade_off: 'May listen on privileged ports in its own namespace.',
      },
    ],
    always_enforced: [
      'Every Linux capability not allowed here is dropped',
      'A 512-process limit',
    ],
    can_edit: true,
    edit_requirement:
      'An instance administrator with projects:write on this project can change container security.',
    ...overrides,
  }
}

function renderSettings() {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({
          defaultOptions: {
            queries: { retry: false },
            mutations: { retry: false },
          },
        })
      }
    >
      <ContainerSecuritySettings projectId={9} />
    </QueryClientProvider>
  )
}

test('shows the setting, its default and what is always enforced', async () => {
  mockApi({ [`GET ${PATH}`]: () => jsonResponse(security()) })
  const screen = renderSettings()
  const toggle = await screen.findByRole('switch', { name: LABEL })
  expect(toggle.getAttribute('aria-checked')).toBe('false')
  expect(screen.getByText('A 512-process limit')).toBeTruthy()
  expect(document.getElementById('container-security')).not.toBeNull()
})

test('allowing a capability needs a confirmation that names the trade-off', async () => {
  const bodies: unknown[] = []
  mockApi({
    [`GET ${PATH}`]: () => jsonResponse(security()),
    [`PUT ${PATH}`]: async (request) => {
      bodies.push(await request.json())
      return jsonResponse(
        security({ allowed_capabilities: ['NET_BIND_SERVICE'] })
      )
    },
  })
  const screen = renderSettings()
  fireEvent.click(await screen.findByRole('switch', { name: LABEL }))
  expect(await screen.findByText(/May listen on privileged ports/)).toBeTruthy()
  expect(bodies).toEqual([])
  const allow = screen.getByRole('button', {
    name: 'Allow NET_BIND_SERVICE',
  }) as HTMLButtonElement
  expect(allow.disabled).toBe(true)
  fireEvent.click(screen.getByRole('checkbox'))
  fireEvent.click(allow)
  await waitFor(() =>
    expect(
      screen.getByRole('switch', { name: LABEL }).getAttribute('aria-checked')
    ).toBe('true')
  )
  expect(bodies).toEqual([
    {
      allowed_capabilities: ['NET_BIND_SERVICE'],
      expected_allowed_capabilities: [],
      acknowledge_risks: true,
    },
  ])
})

test('removing it again saves straight away, without a confirmation', async () => {
  const bodies: unknown[] = []
  mockApi({
    [`GET ${PATH}`]: () =>
      jsonResponse(security({ allowed_capabilities: ['NET_BIND_SERVICE'] })),
    [`PUT ${PATH}`]: async (request) => {
      bodies.push(await request.json())
      return jsonResponse(security())
    },
  })
  const screen = renderSettings()
  fireEvent.click(await screen.findByRole('switch', { name: LABEL }))
  await waitFor(() => expect(bodies.length).toBe(1))
  expect(bodies[0]).toEqual({
    allowed_capabilities: [],
    expected_allowed_capabilities: ['NET_BIND_SERVICE'],
    acknowledge_risks: false,
  })
  expect(screen.queryByRole('alertdialog')).toBeNull()
})

test('a caller who cannot change it sees who can, and a disabled control', async () => {
  const api = mockApi({
    [`GET ${PATH}`]: () => jsonResponse(security({ can_edit: false })),
  })
  const screen = renderSettings()
  const toggle = (await screen.findByRole('switch', {
    name: LABEL,
  })) as HTMLButtonElement
  expect(toggle.disabled).toBe(true)
  expect(
    screen.getByText(/An instance administrator with projects:write/)
  ).toBeTruthy()
  expect(api.called('PUT', PATH)).toBe(false)
})
