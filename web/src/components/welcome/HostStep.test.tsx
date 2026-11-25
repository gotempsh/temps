// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import type { HostFacts } from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { HostStep } from './HostStep'

installBrowserWindow()
afterEach(cleanup)

const GB = 1024 ** 3
const facts = (running: boolean): HostFacts => ({
  hostname: 'box-01',
  cpu_cores: 2,
  memory: { total_bytes: 4 * GB, available_bytes: 3 * GB },
  swap_total_bytes: 0,
  disk: { mount_point: '/', total_bytes: 80 * GB, available_bytes: 60 * GB },
  docker: { running, version: '27.3.1' },
  ports: [
    { port: 80, holder: 'free' },
    { port: 443, holder: 'free' },
  ],
  lan_ip: '192.168.1.20',
  public_ip: '203.0.113.7',
  tailscale: { running: false, magic_dns: false },
  inbound: { checked: false, note: 'Not checked from outside.' },
})

function renderStep(running: boolean) {
  let continued = 0
  const view = render(
    <HostStep
      facts={facts(running)}
      isPending={false}
      error={null}
      onRecheck={() => {}}
      rechecking={false}
      saving={false}
      onContinue={() => {
        continued += 1
      }}
    />
  )
  return { view, continued: () => continued }
}

test('a healthy machine continues', () => {
  const { view, continued } = renderStep(true)
  expect(view.getByRole('heading', { name: 'This is box-01' })).toBeTruthy()
  fireEvent.click(view.getByRole('button', { name: 'Continue' }))
  expect(continued()).toBe(1)
  expect(view.queryByText(/Start Docker first/)).toBeNull()
})

test('with Docker stopped, Continue says why and does nothing', () => {
  const { view, continued } = renderStep(false)
  const button = view.getByRole('button', { name: 'Continue' })
  expect(button.getAttribute('aria-disabled')).toBe('true')
  // The reason is on the page, not only in a tooltip a phone can't show.
  const reason = view.getByText('Start Docker first: apps run as containers.')
  expect(button.getAttribute('aria-describedby')).toBe(reason.id)
  fireEvent.click(button)
  expect(continued()).toBe(0)
})
