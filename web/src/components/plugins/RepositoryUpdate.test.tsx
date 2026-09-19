// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { RepositoryUpdate, RepositoryUpdateButton } from './RepositoryUpdate'
import { PLUGINS_QUERY_KEY } from '@/hooks/usePlugins'

test('update shows the stored directory and ref without offering a source-path override', () => {
  const client = new QueryClient()
  client.setQueryData([...PLUGINS_QUERY_KEY, 'demo', 'source'], {
    configured: true,
    setup_path: '/settings/plugins',
    source: {
      kind: 'github',
      repository_url: 'https://github.com/example/plugins',
      path: 'plugins/demo',
      ref_name: 'release/v1',
      commit: 'a'.repeat(40),
      version: '1.0.0',
      builder_image: 'fixture',
    },
  })
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <RepositoryUpdate
        name="demo"
        disabled={false}
        onSensitiveError={() => false}
      />
    </QueryClientProvider>
  )
  expect(markup).toContain('plugins/demo')
  expect(markup).toContain('Keep release/v1')
  expect(markup).toContain('Update branch, tag, or commit')
  expect(markup).toContain('name="ref_name"')
  expect(markup).not.toContain('name="path"')
  expect(markup).toContain('type="submit"')
})

test('plugins without a repository source show manual update status instead of an action', () => {
  const client = new QueryClient()
  client.setQueryData([...PLUGINS_QUERY_KEY, 'manual', 'source'], {
    source: null,
  })
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <RepositoryUpdateButton
        name="manual"
        disabled={false}
        onClick={() => {}}
      />
    </QueryClientProvider>
  )
  expect(markup).toContain('Manual update')
  expect(markup).toContain('no GitHub source')
  expect(markup).not.toContain('<button')
})
