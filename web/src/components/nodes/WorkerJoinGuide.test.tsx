// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { WorkerJoinGuide } from './WorkerJoinGuide'

afterEach(cleanup)

const RELEASE = 'v0.1.0-beta.60'
/** What `POST /nodes/pairings` returns: the server's command, pinned. */
const PAIRING_COMMAND = [
  ` (set -o pipefail; curl -fsSL https://temps.sh/install.sh | sudo -H bash -s -- ${RELEASE}) && \\`,
  "printf '%s\\n' 'tpair1.secret' | sudo -H /root/.temps/bin/temps join --pair - && \\",
  'sudo -H /root/.temps/bin/temps agent service install',
].join('\n')

const pairing = {
  id: 5,
  name: 'worker-1',
  node_endpoint: '203.0.113.20:51820',
  mesh_address: '10.201.0.2',
  status: 'waiting',
  last_error: null,
  last_rejection: null,
  last_attempt_at: null,
  expires_at: '2099-01-01T00:00:00Z',
  node_id: null,
  created_at: '2026-11-06T08:00:00Z',
}

function setup({ version = RELEASE }: { version?: string } = {}) {
  const api = mockApi({
    'GET /nodes/wireguard': () =>
      jsonResponse({
        state: 'ready',
        cidr: '10.201.0.0/16',
        pool_cidr: '10.205.0.0/16',
        listen_port: 51820,
        // A control plane behind NAT: pairing is the default way in.
        join_url: 'http://localhost:8080',
        can_enable: false,
        ca_rotation_required: false,
        control_plane: {
          address: '10.201.0.1',
          endpoint: null,
          endpoint_is_private: false,
        },
        hub: { target: { kind: 'control_plane' }, name: 'control plane' },
        nodes: [],
      }),
    'GET /nodes/pairings': () => jsonResponse({ pairings: [pairing] }),
    'POST /nodes/pairings': () =>
      jsonResponse({ pairing, join_command: PAIRING_COMMAND }, 201),
    'GET /settings/update': () =>
      jsonResponse({ current_version: version, phase: 'idle' }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <WorkerJoinGuide token="tk_join" />
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { api }
}

async function createPairing() {
  fireEvent.change(await screen.findByLabelText('Worker public IP'), {
    target: { value: '203.0.113.20' },
  })
  fireEvent.click(screen.getByRole('button', { name: /Create pairing/ }))
  return waitFor(() => {
    const shown = document.querySelector('pre')
    expect(shown?.textContent).toBe(PAIRING_COMMAND)
    return shown
  })
}

test('the pairing shows the server command whole, with no separate install step', async () => {
  setup()
  await createPairing()
  const text = document.body.textContent ?? ''
  // The command installs Temps itself, pinned, as the Add worker panel does.
  expect(text).not.toContain('Install Temps on the worker')
  expect(text).not.toContain('curl -fsSL https://temps.sh/install.sh | bash\n')
  expect(text).toContain(`Installs ${RELEASE}, the version this server runs`)
  // sudo is on the lines that need it, so it does not ask for a root shell.
  expect(text).not.toContain(', as root')
  expect(text).not.toContain('Put sudo right before')
  expect(text).toContain('starts the agent service')
})

test('a Mac is sent to the outbound join instead of pairing', async () => {
  setup()
  await screen.findByLabelText('Worker public IP')
  const text = document.body.textContent ?? ''
  expect(text).toContain('A Mac cannot be paired')
  expect(text).toContain('choose Mac')
})

test('an unreadable server version says the install is not pinned', async () => {
  setup({ version: 'dev' })
  await createPairing()
  await waitFor(() =>
    expect(document.body.textContent).toContain(
      "This server's release could not be read"
    )
  )
})
