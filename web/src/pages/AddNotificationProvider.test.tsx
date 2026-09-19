// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { SettingsLayout } from '@/components/settings/SettingsLayout'
import { AddNotificationProvider } from './AddNotificationProvider'

function renderPage(search = '') {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/settings/notifications/new' + search]}>
        <BreadcrumbProvider>
          <AddNotificationProvider />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  client.clear()
  return markup
}

test('provider choices are native buttons and progress has readable labels', () => {
  const markup = renderPage()
  expect(markup).toContain('Setup progress')
  expect(markup).toContain('How should notifications reach you?')
  expect(markup).toMatch(/<button[^>]*>[\s\S]*?Slack/)
  expect(markup.match(/<h1/g)).toHaveLength(1)
})

test('settings route wraps provider wizard in exactly one page container', () => {
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <MemoryRouter initialEntries={['/settings/notifications/new']}>
        <BreadcrumbProvider>
          <Routes>
            <Route path="/settings" element={<SettingsLayout />}>
              <Route
                path="notifications/new"
                element={<AddNotificationProvider />}
              />
            </Route>
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  expect(markup.match(/data-page-container/g)).toHaveLength(1)
  expect(markup).not.toContain('lg:w-4/5')
})

test('a valid URL restores configuration, without credentials in the URL', () => {
  const markup = renderPage('?step=configuration&provider=slack')
  expect(markup).toContain('Configure Slack')
  expect(markup).toContain('form="add-notification-provider-form"')
  expect(markup).toContain('Webhook URL')
  expect(markup).not.toContain('Provider Type')
})

test('unavailable providers and forged completion return to provider selection', () => {
  expect(renderPage('?step=configuration&provider=coming-soon')).toContain(
    'How should notifications reach you?'
  )
  expect(renderPage('?step=complete&provider=slack')).not.toContain(
    'Ready to send notifications'
  )
})
