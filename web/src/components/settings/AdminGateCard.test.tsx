// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { getAdminGateOptions } from '@/api/client/@tanstack/react-query.gen'
import type { AdminGateResponse } from '@/api/client/types.gen'
import { AdminGateCard } from './AdminGateCard'
import { adminGateIsOpen, suggestedConsoleHost } from './admin-gate'

function render(gate: AdminGateResponse) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  queryClient.setQueryData(getAdminGateOptions().queryKey, gate)
  const html = renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      <AdminGateCard />
    </QueryClientProvider>
  )
  queryClient.clear()
  return html
}

const openGate: AdminGateResponse = {
  allowed_ips: [],
  allowed_hosts: [],
  trust_forwarded_for: false,
  source: 'default',
  editable: true,
}

describe('adminGateIsOpen', () => {
  test('no rules means open', () => {
    expect(adminGateIsOpen({ allowed_ips: [], allowed_hosts: [] })).toBe(true)
    expect(adminGateIsOpen({})).toBe(true)
  })

  test('any host or IP rule closes it', () => {
    expect(adminGateIsOpen({ allowed_hosts: ['console.example.com'] })).toBe(
      false
    )
    expect(adminGateIsOpen({ allowed_ips: ['10.0.0.0/8'] })).toBe(false)
  })
})

describe('suggestedConsoleHost', () => {
  test('offers a real hostname, lowercased', () => {
    expect(suggestedConsoleHost('Console.Example.com')).toBe(
      'console.example.com'
    )
  })

  test('never offers an IP literal or loopback name', () => {
    expect(suggestedConsoleHost('203.0.113.7')).toBeNull()
    expect(suggestedConsoleHost('[2001:db8::1]')).toBeNull()
    expect(suggestedConsoleHost('2001:db8::1')).toBeNull()
    expect(suggestedConsoleHost('localhost')).toBeNull()
    expect(suggestedConsoleHost('app.localhost')).toBeNull()
    expect(suggestedConsoleHost('')).toBeNull()
  })
})

describe('AdminGateCard', () => {
  test('says the console is open when no rules are saved', () => {
    const html = render(openGate)
    expect(html).toContain('The console answers on any hostname and address')
    // Outside a browser there is no current hostname to offer.
    expect(html).toContain('Open the console through its domain name')
  })

  test('shows no open notice once a rule is saved', () => {
    const html = render({
      ...openGate,
      allowed_hosts: ['console.example.com'],
      source: 'db',
    })
    expect(html).not.toContain('The console answers on any hostname')
  })

  test('env-managed rules are read-only and show no one-click fix', () => {
    const html = render({ ...openGate, source: 'env', editable: false })
    expect(html).toContain('Managed by environment variables')
    expect(html).not.toContain('The console answers on any hostname')
  })
})
