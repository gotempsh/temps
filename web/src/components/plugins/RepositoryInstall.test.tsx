// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { afterEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  CONSOLE_MAX_SCROLL,
  ConsoleShell,
  NEAR_THE_END,
  expectOnlyConsoleScrolled,
} from '@/test/console-shell'
import { installBrowserWindow } from '@/test/dom'
import { installFakeScrollLayout } from '@/test/fake-scroll-layout'
import { RepositoryInstall } from './RepositoryInstall'

installBrowserWindow()
const layout = installFakeScrollLayout()
afterEach(cleanup)

const SELECTION = {
  name: 'example',
  repository: 'https://github.com/example/plugin',
  commit: 'a'.repeat(40),
  path: 'plugins/demo',
  ref: 'release/v1',
}

test('catalog review shows the pinned commit and keeps explicit trust without custom overrides', () => {
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <RepositoryInstall
        disabled={false}
        onSensitiveError={() => false}
        selection={{
          name: 'example',
          repository: 'https://github.com/example/plugin',
          commit: 'a'.repeat(40),
          path: 'plugins/demo',
          ref: 'release/v1',
        }}
      />
    </QueryClientProvider>
  )
  expect(markup).toContain('Review installation')
  expect(markup).toContain('a'.repeat(40))
  expect(markup).toContain('Back to catalog')
  expect(markup).toContain('plugins/demo')
  expect(markup).toContain('release/v1')
  expect(markup).toContain('aria-checked="false"')
  expect(markup).toContain('I trust this repository')
  expect(markup).not.toContain('Name and revision')
  expect(markup).not.toContain('Paste a repository URL')
})

test('choosing a catalog entry centres the review in the page, not the shell', () => {
  // Centring a form near the end of the catalog asks for more than the page
  // can scroll; the rest must not come out of the shell.
  layout.place('#repository-install-title', NEAR_THE_END)
  render(
    <QueryClientProvider client={new QueryClient()}>
      <ConsoleShell>
        <RepositoryInstall
          disabled={false}
          onSensitiveError={() => false}
          selection={SELECTION}
        />
      </ConsoleShell>
    </QueryClientProvider>
  )
  expectOnlyConsoleScrolled(layout, CONSOLE_MAX_SCROLL)
})

test('the centre of the review lines up with the centre of the page', () => {
  layout.place('#repository-install-title', { top: 900, height: 40 })
  render(
    <QueryClientProvider client={new QueryClient()}>
      <ConsoleShell>
        <RepositoryInstall
          disabled={false}
          onSensitiveError={() => false}
          selection={SELECTION}
        />
      </ConsoleShell>
    </QueryClientProvider>
  )
  // 900 + 40 / 2 - 700 / 2
  expectOnlyConsoleScrolled(layout, 570)
})
