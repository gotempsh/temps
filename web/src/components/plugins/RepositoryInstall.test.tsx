// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { RepositoryInstall } from './RepositoryInstall'

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
