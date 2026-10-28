// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { certificateSettingsErrors } from './certificate-settings'
import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { getCertificateSettingsOptions } from '@/api/client/@tanstack/react-query.gen'
import { DomainCertificateSettingsPanel } from './DomainCertificateSettingsPanel'

const settings = {
  challenge_type: 'http-01',
  renewal_days: 30,
  alert_days: 7,
  alerts_enabled: true,
}

test('certificate policy rejects wildcard HTTP and invalid day ranges', () => {
  expect(
    certificateSettingsErrors(settings, true)['Validation method']
  ).toContain('DNS-01')
  expect(
    Object.values(certificateSettingsErrors(settings, false)).some(Boolean)
  ).toBe(false)
  for (const days of [0, 91, 1.5, NaN]) {
    expect(
      certificateSettingsErrors({ ...settings, renewal_days: days }, false)[
        'Renewal window'
      ]
    ).toBeDefined()
    expect(
      certificateSettingsErrors({ ...settings, alert_days: days }, false)[
        'Alert window'
      ]
    ).toBeDefined()
  }
  expect(
    certificateSettingsErrors({ ...settings, alert_days: null }, false)[
      'Alert window'
    ]
  ).toBeUndefined()
})

test('domain exposes the policy and explains when changes apply', () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData(
    getCertificateSettingsOptions({ path: { domain_id: 1 } }).queryKey,
    settings
  )
  const html = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <DomainCertificateSettingsPanel domainId={1} wildcard={false} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  for (const text of [
    'Certificate settings',
    'current certificate keeps serving traffic',
    'pending order keeps its original',
    'Renew before expiry (days)',
    'Alert before expiry (days)',
    'Manage instance alert preferences',
    'Save certificate settings',
  ])
    expect(html).toContain(text)
  expect(html).not.toContain('<h1')
  expect(html).not.toContain('data-page-container')
  expect(html).toContain('Validation &amp; renewal')
  expect(html).toContain('<h3')
  client.clear()
})
