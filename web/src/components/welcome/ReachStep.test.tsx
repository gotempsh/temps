// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { installBrowserWindow } from '@/test/dom'
import { mockApi } from '@/test/fetch-mock'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { BoxFacts } from './access'
import { ReachStep } from './ReachStep'

installBrowserWindow()
afterEach(cleanup)

const FACTS: BoxFacts = {
  publicIp: '203.0.113.7',
  inbound: null,
  lanIp: '192.168.1.20',
  tailscale: {
    ip: '100.101.102.103',
    machine: 'box-01',
    tailnet: 'tail0000.ts.net',
  },
  privateNetwork: {
    label: 'Tailscale',
    address: '100.101.102.103',
    listen: 'tailscale',
    admit: ['100.64.0.0/10', 'fd7a:115c:a1e0::/48'],
  },
}

test('Do this later waits while the plan is being saved', async () => {
  let writes = 0
  const api = mockApi({
    // The save never finishes, so it stays pending.
    'PATCH /settings/onboarding': () => {
      writes += 1
      return new Promise<Response>(() => {})
    },
  })
  try {
    let continued = 0
    const client = new QueryClient({
      defaultOptions: {
        queries: { retry: false },
        mutations: { retry: false },
      },
    })
    const view = render(
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <ReachStep facts={FACTS} onContinue={() => (continued += 1)} />
        </MemoryRouter>
      </QueryClientProvider>
    )
    // Anyone on the internet, through a tunnel, every app under one domain.
    for (let i = 0; i < 3; i++) {
      fireEvent.click(view.getByRole('button', { name: 'Continue' }))
    }
    fireEvent.change(view.getByRole('textbox'), {
      target: { value: 'apps.example.com' },
    })
    fireEvent.click(view.getByRole('button', { name: 'Continue' }))
    expect(view.getByText('Here is the plan')).toBeTruthy()

    fireEvent.click(
      view.getByRole('button', { name: 'Save and set up the tunnel' })
    )
    await waitFor(() => expect(writes).toBe(1))
    const later = view.getByRole('button', { name: 'Do this later' })
    await waitFor(() =>
      expect((later as HTMLButtonElement).disabled).toBe(true)
    )
    fireEvent.click(later)
    // The deferral is not written alongside the save, and the step stays.
    await new Promise((resolve) => setTimeout(resolve, 20))
    expect(writes).toBe(1)
    expect(continued).toBe(0)
  } finally {
    api.restore()
  }
})
