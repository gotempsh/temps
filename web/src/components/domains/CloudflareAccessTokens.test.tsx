// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render as renderDom, waitFor } from '@testing-library/react'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type { AccessProviderResponse } from '@/api/client'
import { deleteBlockedReason } from '@/lib/cloudflare-access'
import {
  CONSOLE_MAX_SCROLL,
  ConsoleShell,
  NEAR_THE_END,
  expectOnlyConsoleScrolled,
} from '@/test/console-shell'
import { installBrowserWindow } from '@/test/dom'
import { installFakeScrollLayout } from '@/test/fake-scroll-layout'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import {
  CloudflareAccessTokens,
  SavedAccessToken,
} from './CloudflareAccessTokens'

installBrowserWindow()
const layout = installFakeScrollLayout()
afterEach(cleanup)

const provider = (addresses: number): AccessProviderResponse => ({
  id: 7,
  name: 'Zero Trust',
  account_id: '0123456789abcdef0123456789abcdef',
  api_token: '***',
  team_domain: 'team.cloudflareaccess.com',
  addresses,
  key_count: 1,
  keys_fetched_at: '2026-10-26T10:00:00Z',
  created_at: '2026-10-26T09:00:00Z',
})

function render(addresses: number, canManage = true): string {
  return renderToStaticMarkup(
    <SavedAccessToken
      provider={provider(addresses)}
      canManage={canManage}
      onDelete={() => {}}
    />
  )
}

describe('SavedAccessToken', () => {
  test('shows why a token in use cannot be deleted, tied to the button', () => {
    const html = render(1)
    expect(html).toContain('Switch it to the Temps sign-in')
    expect(html).toContain('id="access-token-7-delete-blocked"')
    expect(html).toContain('aria-describedby="access-token-7-delete-blocked"')
    // Visible text, not a tooltip a disabled button never shows.
    expect(html).not.toContain(`title="${deleteBlockedReason(1)}`)
  })

  test('a token protecting nothing has no blocked reason', () => {
    const html = render(0)
    expect(html).not.toContain('delete-blocked')
    expect(html).not.toContain('Switch')
  })

  test('someone who cannot delete is not told how to unblock it', () => {
    expect(render(2, false)).not.toContain('delete-blocked')
  })
})

describe('CloudflareAccessTokens', () => {
  test('a #cloudflare-access link scrolls the page to the card and leaves the shell in place', async () => {
    mockApi({ 'GET /cloudflare-access/providers': () => jsonResponse([]) })
    layout.place('#cloudflare-access', NEAR_THE_END)
    renderDom(
      <QueryClientProvider
        client={
          new QueryClient({ defaultOptions: { queries: { retry: false } } })
        }
      >
        <MemoryRouter initialEntries={['/domains#cloudflare-access']}>
          <ConsoleShell>
            <CloudflareAccessTokens canManage />
          </ConsoleShell>
        </MemoryRouter>
      </QueryClientProvider>
    )
    await waitFor(() => expect(layout.scrolls.length).toBeGreaterThan(0))
    expectOnlyConsoleScrolled(layout, CONSOLE_MAX_SCROLL)
  })
})
