// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render } from '@testing-library/react'
import {
  CONSOLE_MAX_SCROLL,
  ConsoleShell,
  NEAR_THE_END,
  expectOnlyConsoleScrolled,
} from '@/test/console-shell'
import { installBrowserWindow } from '@/test/dom'
import { installFakeScrollLayout } from '@/test/fake-scroll-layout'
import { mockApi } from '@/test/fetch-mock'
import { ComposeSecuritySettings } from './ComposeSecuritySettings'

installBrowserWindow()
const layout = installFakeScrollLayout()
afterEach(cleanup)

function renderPanel(focusCheck: string | null) {
  // The checks never load: the panel scrolls before they do.
  mockApi({})
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <ConsoleShell>
        <ComposeSecuritySettings projectId={1} focusCheck={focusCheck} />
      </ConsoleShell>
    </QueryClientProvider>
  )
}

test('a link to one check scrolls the page to the panel and leaves the shell in place', () => {
  // The panel is the last card on the security settings page.
  layout.place('#compose-security', NEAR_THE_END)
  renderPanel('privileged')
  expectOnlyConsoleScrolled(layout, CONSOLE_MAX_SCROLL)
})

test('without a linked check nothing scrolls', () => {
  layout.place('#compose-security', NEAR_THE_END)
  renderPanel(null)
  expect(layout.scrolls).toEqual([])
  expect(layout.intoView).toEqual([])
})
