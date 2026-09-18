// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { AddEmailProvider, createProviderSchema } from './AddEmailProvider'

function renderPage(search: string) {
  const client = new QueryClient()
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/email/providers/new' + search]}>
        <BreadcrumbProvider>
          <AddEmailProvider />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  client.clear()
  return markup
}

test.each([
  ['ses', 'Secret Access Key'],
  ['scaleway', 'Project ID'],
  ['smtp', 'SMTP'],
])(
  'restores %s configuration with a shared action footer',
  (provider, label) => {
    const markup = renderPage(`?step=configure&provider=${provider}`)
    expect(markup).toContain(label)
    expect(markup).toContain('form="add-email-provider-form"')
    expect(markup.match(/<h1/g)).toHaveLength(1)
  }
)

test('unknown providers return to selection', () => {
  expect(renderPage('?step=configure&provider=unknown')).toContain(
    'Choose a provider type'
  )
})

test('SMTP still validates host, port, and paired credentials', () => {
  const valid = {
    name: 'Mail',
    provider_type: 'smtp',
    region: 'custom',
    smtp_host: 'smtp.example.test',
    smtp_port: 587,
  }
  expect(createProviderSchema.safeParse(valid).success).toBe(true)
  expect(
    createProviderSchema.safeParse({ ...valid, smtp_host: '' }).success
  ).toBe(false)
  expect(
    createProviderSchema.safeParse({ ...valid, smtp_port: 0 }).success
  ).toBe(false)
  expect(
    createProviderSchema.safeParse({ ...valid, smtp_username: 'user' }).success
  ).toBe(false)
})

test('SES and Scaleway require their own credentials', () => {
  expect(
    createProviderSchema.safeParse({
      name: 'Mail',
      provider_type: 'ses',
      region: 'us-east-1',
    }).success
  ).toBe(false)
  expect(
    createProviderSchema.safeParse({
      name: 'Mail',
      provider_type: 'scaleway',
      region: 'fr-par',
    }).success
  ).toBe(false)
})
