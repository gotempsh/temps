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
import type { WorkerInstall } from '@/lib/worker-join-script'
import { AddWorkerNode } from './AddWorkerNode'

afterEach(cleanup)

const RELEASE = 'v0.1.0-nightly.20261104.0123abcde'

function setup({
  version = RELEASE,
  workerInstall,
}: { version?: string; workerInstall?: WorkerInstall } = {}) {
  let minted = 0
  const api = mockApi({
    'GET /nodes/wireguard': () =>
      jsonResponse({
        state: 'ready',
        cidr: '10.201.0.0/16',
        pool_cidr: '10.201.0.0/16',
        listen_port: 51820,
        join_url: 'https://cp.example.com',
        can_enable: false,
        ca_rotation_required: false,
        control_plane: {
          address: '10.201.0.1',
          endpoint: '203.0.113.10:51820',
          endpoint_is_private: false,
        },
        hub: { target: { kind: 'control_plane' }, name: 'control plane' },
        nodes: [],
      }),
    'GET /settings/cluster-network': () =>
      jsonResponse({
        can_change: false,
        change_blocked_by: [],
        compute_pool_cidr: '10.205.0.0/16',
        default_compute_pool_cidr: '10.205.0.0/16',
        mesh_enabled: true,
        mesh_pool_cidr: '10.201.0.0/16',
        nodes: [],
        overlay_network_name: 'temps',
        pool_confirmed: true,
        subnet_prefix_len: 24,
        control_plane: {},
      }),
    'GET /internal/nodes': () => jsonResponse({ nodes: [] }),
    'GET /settings/update': () =>
      jsonResponse({
        current_version: version,
        worker_install: workerInstall,
        phase: 'idle',
      }),
    'POST /settings/enrollment-tokens': () => {
      minted += 1
      return jsonResponse({
        id: minted,
        token: `tk_enroll_${minted}`,
        expires_at: '2099-01-01T00:00:00Z',
        max_uses: 1,
        message: 'minted',
      })
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <AddWorkerNode otherWays={null} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { api }
}

/** The command block's text: one command, steps joined with `&& \`. */
async function shownCommand(): Promise<string> {
  const block = await screen.findByText(/sudo -H bash -s --|sudo -H bash\)/)
  return block.textContent ?? ''
}

test('ticking "move it here" before creating the first command adds --force to it', async () => {
  setup()
  const force = await screen.findByRole('checkbox', {
    name: /already a worker of another Temps cluster/,
  })
  fireEvent.click(force)
  await screen.findByText('The old cluster loses this worker')

  fireEvent.click(screen.getByRole('button', { name: 'Create join command' }))
  const command = await shownCommand()
  expect(command).toContain(
    'sudo -H /root/.temps/bin/temps join https://cp.example.com tk_enroll_1 --ingress delegated --force && \\'
  )
})

test('the first command is pinned to the release this server runs and uses sudo', async () => {
  setup()
  fireEvent.click(
    await screen.findByRole('button', { name: 'Create join command' })
  )
  const command = await shownCommand()
  expect(command).toContain(
    `(set -o pipefail; curl -fsSL https://temps.sh/install.sh | sudo -H bash -s -- ${RELEASE}) && \\`
  )
  expect(command).toContain(
    'sudo -H /root/.temps/bin/temps join https://cp.example.com tk_enroll_1 --ingress delegated && \\'
  )
  expect(command).not.toContain('--force')
  expect(command).toContain(
    'sudo -H /root/.temps/bin/temps agent service install'
  )
  expect(screen.getByText(/the version this server runs/)).toBeTruthy()
  expect(screen.getByText('What does the new machine run?')).toBeTruthy()
})

test('"Create another" is for another machine, so it drops a --force chosen for the last one', async () => {
  setup()
  fireEvent.click(
    await screen.findByRole('checkbox', {
      name: /already a worker of another Temps cluster/,
    })
  )
  fireEvent.click(screen.getByRole('button', { name: 'Create join command' }))
  expect(await shownCommand()).toContain('--force')

  fireEvent.click(screen.getByRole('button', { name: 'Create another' }))
  await waitFor(async () =>
    expect(await shownCommand()).toContain('tk_enroll_2')
  )
  expect(await shownCommand()).not.toContain('--force')
})

test('a version that is not a release tag is not pinned, and the panel says so', async () => {
  setup({ version: 'dev' })
  fireEvent.click(
    await screen.findByRole('button', { name: 'Create join command' })
  )
  const command = await shownCommand()
  expect(command).toContain(
    '(set -o pipefail; curl -fsSL https://temps.sh/install.sh | sudo -H bash) && \\'
  )
  expect(
    screen.getByText(/newest stable release, which may not match it/)
  ).toBeTruthy()
})

test('a build between releases installs the newest nightly, never the older release', async () => {
  // The server reports the release before its build as its version.
  setup({
    version: 'v0.1.0-beta.61',
    workerInstall: { kind: 'nightly', after: 'v0.1.0-beta.61' },
  })
  fireEvent.click(
    await screen.findByRole('button', { name: 'Create join command' })
  )
  const command = await shownCommand()
  expect(command).toContain(
    '(set -o pipefail; curl -fsSL https://temps.sh/install.sh | sudo -H bash -s -- --channel nightly) && \\'
  )
  expect(command).not.toContain('bash -s -- v0.1.0-beta.61')
  expect(screen.getByText(/which no release contains/)).toBeTruthy()
})

test('a Mac cannot take ingress on its main address or Tailscale, and the command falls back to delegated', async () => {
  setup()
  const fixed = await screen.findByRole('radio', {
    name: /Yes, on its main address/,
  })
  fireEvent.click(fixed)
  expect(fixed.getAttribute('aria-checked')).toBe('true')

  fireEvent.click(screen.getByRole('radio', { name: /^Mac/ }))
  await waitFor(() =>
    expect(
      screen
        .getByRole('radio', { name: /Yes, on its main address/ })
        .hasAttribute('disabled')
    ).toBe(true)
  )
  expect(
    screen
      .getByRole('radio', { name: /Only my private network/ })
      .hasAttribute('disabled')
  ).toBe(true)
  expect(screen.getByText(/cannot open ports 80 and 443/)).toBeTruthy()
  expect(
    screen
      .getByRole('radio', { name: /^Yes, managed from this console/ })
      .getAttribute('aria-checked')
  ).toBe('true')

  fireEvent.click(screen.getByRole('button', { name: 'Create join command' }))
  const block = await screen.findByText(/~\/\.temps\/bin\/temps join/)
  expect(block.textContent).toContain(
    '~/.temps/bin/temps join https://cp.example.com tk_enroll_1 --ingress delegated && \\'
  )
  expect(block.textContent).not.toContain('sudo')
})
