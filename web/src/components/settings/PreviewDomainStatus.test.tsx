// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { PreviewDomainStatusResponse } from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { PreviewDomainStatusView } from './PreviewDomainStatus'

installBrowserWindow()
afterEach(cleanup)

function renderStatus(
  status: PreviewDomainStatusResponse['status'],
  tunnel: string | null = null
) {
  return render(
    <MemoryRouter>
      <PreviewDomainStatusView
        status={{
          preview_domain: 'localho.st',
          status,
          reason: 'Names under localho.st resolve to 127.0.0.1.',
          tunnel_environment_hosts: tunnel,
          checked_at: '2026-10-25T10:00:00Z',
        }}
      />
    </MemoryRouter>
  )
}

test('a local-only preview domain states the fact and the fixes, without an alert', () => {
  const { container } = renderStatus('local_only')
  expect(container.querySelector('[role="alert"]')).toBeNull()
  expect(container.textContent).toContain(
    'Only this machine can open these names'
  )
  expect(container.textContent).toContain('resolve to 127.0.0.1')
  expect(container.querySelector('a')?.getAttribute('href')).toBe(
    '/settings/nodes/ingress'
  )
})

test('a usable preview domain is a quiet confirmation, not a warning', () => {
  const { container } = renderStatus('usable')
  expect(container.textContent).toContain('Anyone can open these names')
  expect(container.querySelector('[role="alert"]')).toBeNull()
})

test('a tunnel publishing every environment makes the local domain no problem', () => {
  const { container } = renderStatus('local_only', 'edge')
  expect(container.textContent).toContain(
    'Tunnel edge gives every environment an address'
  )
  expect(container.querySelector('[role="alert"]')).toBeNull()
})

test('a private address is a deliberate setup, not a warning', () => {
  const { container } = renderStatus('no_way_in')
  expect(container.textContent).toContain('Not reachable from the internet')
  expect(container.querySelector('[role="alert"]')).toBeNull()
  expect(container.querySelector('a')?.getAttribute('href')).toBe(
    '/settings/nodes/ingress'
  )
})

test('names that do not resolve at all still warn', () => {
  const { container } = renderStatus('unresolved')
  expect(container.querySelector('[role="alert"]')).not.toBeNull()
  expect(container.textContent).toContain('These names do not resolve yet')
})
