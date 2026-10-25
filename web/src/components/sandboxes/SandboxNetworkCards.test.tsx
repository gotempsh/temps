// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import type { ReactNode } from 'react'
import { getSandboxQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { SandboxEgressCard } from './SandboxEgressCard'
import { SandboxPreviewAccessCard } from './SandboxPreviewAccessCard'

installBrowserWindow()
afterEach(cleanup)

function withClient(children: ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const detailKey = getSandboxQueryKey({ path: { id: 'sbx_1' } })
  client.setQueryData(detailKey, { sandbox: {} })
  return {
    client,
    detailKey,
    view: render(
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    ),
  }
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 100))

test('preview access: saves the chosen mode with the allow-list', async () => {
  let body: unknown = null
  const api = mockApi({
    'PUT /v1/sandboxes/sbx_1/preview-access': async (request) => {
      body = await request.json()
      return jsonResponse({
        id: 'sbx_1',
        preview_access: 'public',
        preview_allowed_cidrs: ['203.0.113.7/32'],
      })
    },
  })
  const { client, detailKey, view } = withClient(
    <SandboxPreviewAccessCard
      sandboxId="sbx_1"
      access="temps_login"
      allowedCidrs={[]}
      hasPassword={false}
    />
  )
  const save = view.getByRole('button', { name: 'Save access' })
  expect(save.hasAttribute('disabled')).toBe(true)

  fireEvent.click(view.getByRole('radio', { name: /Anyone with the link/ }))
  fireEvent.change(view.getByLabelText('Allowed networks'), {
    target: { value: '203.0.113.7\n' },
  })
  fireEvent.click(save)
  await settle()

  expect(body).toEqual({ access: 'public', allowed_cidrs: ['203.0.113.7'] })
  expect(client.getQueryState(detailKey)?.isInvalidated).toBe(true)
  api.restore()
})

test('preview access: will not choose password before one is set, nor send a bad CIDR', () => {
  const api = mockApi({})
  const { view } = withClient(
    <SandboxPreviewAccessCard
      sandboxId="sbx_1"
      access="temps_login"
      allowedCidrs={[]}
      hasPassword={false}
    />
  )
  fireEvent.click(view.getByRole('radio', { name: /Anyone with the password/ }))
  expect(view.getByRole('alert').textContent).toContain(
    'Set a preview password below'
  )
  expect(
    view.getByRole('button', { name: 'Save access' }).hasAttribute('disabled')
  ).toBe(true)

  fireEvent.click(view.getByRole('radio', { name: /Only you, signed in/ }))
  fireEvent.change(view.getByLabelText('Allowed networks'), {
    target: { value: 'office-wifi' },
  })
  expect(view.getByRole('alert').textContent).toContain(
    '"office-wifi" is not an IP address or CIDR'
  )
  expect(
    view.getByRole('button', { name: 'Save access' }).hasAttribute('disabled')
  ).toBe(true)
  api.restore()
})

test('egress: switching to restricted sends the listed destinations', async () => {
  let body: unknown = null
  const api = mockApi({
    'PUT /v1/sandboxes/sbx_1/egress-policy': async (request) => {
      body = await request.json()
      return jsonResponse({})
    },
  })
  const { client, detailKey, view } = withClient(
    <SandboxEgressCard
      sandboxId="sbx_1"
      policy={{ mode: 'internet', rules: [] }}
      backend="docker"
    />
  )
  const save = view.getByRole('button', { name: 'Save outbound network' })
  expect(save.hasAttribute('disabled')).toBe(true)

  fireEvent.click(view.getByRole('radio', { name: /Only listed destinations/ }))
  expect(view.container.textContent).toContain(
    'the sandbox can reach nothing but Temps'
  )
  fireEvent.click(view.getByRole('button', { name: 'Add destination' }))
  fireEvent.change(view.getByLabelText('Rule 1 destination'), {
    target: { value: 'registry.npmjs.org' },
  })
  fireEvent.click(view.getByRole('button', { name: 'Add destination' }))
  fireEvent.change(view.getByLabelText('Rule 2 type'), {
    target: { value: 'cidr' },
  })
  fireEvent.change(view.getByLabelText('Rule 2 destination'), {
    target: { value: '198.51.100.0/24' },
  })
  fireEvent.change(view.getByLabelText('Rule 2 ports'), {
    target: { value: '8443' },
  })
  fireEvent.click(save)
  await settle()

  expect(body).toEqual({
    mode: 'restricted',
    rules: [
      { host: 'registry.npmjs.org', ports: [] },
      { cidr: '198.51.100.0/24', ports: [8443] },
    ],
  })
  expect(client.getQueryState(detailKey)?.isInvalidated).toBe(true)
  api.restore()
})

test('egress: explains a backend that cannot restrict instead of failing on save', () => {
  const api = mockApi({})
  const { view } = withClient(
    <SandboxEgressCard
      sandboxId="sbx_1"
      policy={{ mode: 'internet', rules: [] }}
      backend="firecracker"
    />
  )
  fireEvent.click(view.getByRole('radio', { name: /Only listed destinations/ }))
  expect(view.getByRole('alert').textContent).toContain(
    'firecracker backend, which cannot limit'
  )
  expect(
    view
      .getByRole('button', { name: 'Save outbound network' })
      .hasAttribute('disabled')
  ).toBe(true)
  expect(api.calls).toHaveLength(0)
  api.restore()
})
