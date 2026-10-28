// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { getCertificateSettingsOptions } from '@/api/client/@tanstack/react-query.gen'
import { DomainCertificateSettingsPanel } from './DomainCertificateSettingsPanel'

installBrowserWindow()
afterEach(cleanup)

test('refetch and failed save preserve edits; successful save submits explicit policy', async () => {
  const initial = {
    challenge_type: 'dns-01',
    renewal_days: 30,
    alert_days: null,
    alerts_enabled: null,
  }
  let failSave = true
  let submitted: unknown
  const api = mockApi({
    'GET /domains/1/certificate-settings': () => jsonResponse(initial),
    'PUT /domains/1/certificate-settings': async (request) => {
      submitted = await request.json()
      return failSave
        ? jsonResponse({ detail: 'Database unavailable' }, 500)
        : jsonResponse(submitted)
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <DomainCertificateSettingsPanel domainId={1} wildcard={false} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  try {
    const renewal = (await view.findByLabelText(
      'Renew before expiry (days)'
    )) as HTMLInputElement
    fireEvent.change(renewal, { target: { value: '20' } })
    const alert = view.getByLabelText(
      'Alert before expiry (days)'
    ) as HTMLInputElement
    fireEvent.change(alert, { target: { value: '7' } })
    await client.invalidateQueries({
      queryKey: getCertificateSettingsOptions({ path: { domain_id: 1 } })
        .queryKey,
    })
    expect(renewal.value).toBe('20')
    fireEvent.click(
      view.getByRole('button', { name: 'Save certificate settings' })
    )
    await view.findByText(/Database unavailable/)
    expect(renewal.value).toBe('20')
    expect(alert.value).toBe('7')
    failSave = false
    fireEvent.click(
      view.getByRole('button', { name: 'Save certificate settings' })
    )
    await waitFor(() =>
      expect(
        view
          .getByRole('button', { name: 'Save certificate settings' })
          .getAttribute('aria-disabled')
      ).toBe('true')
    )
    expect(submitted).toEqual({ ...initial, renewal_days: 20, alert_days: 7 })
  } finally {
    client.clear()
    api.restore()
  }
})
