// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { RepositoryCatalogPlugin } from '@/api/client/types.gen'
import { RepositoryCatalog, filterRepositoryCatalog } from './RepositoryCatalog'

const plugin: RepositoryCatalogPlugin = {
  name: 'example',
  title: 'Example plugin',
  summary: 'Inspect deployment health.',
  description: 'Inspect deployment health.',
  author: 'Example team',
  category: 'Development',
  repository: 'https://github.com/example/plugin',
  path: 'plugins/demo',
  ref: 'release/v1',
  commit: 'a'.repeat(40),
  latestVersion: '1.0.0',
  logoUrl: null,
  docsUrl: null,
  readmeUrl: null,
  screenshots: [],
  platforms: ['darwin-arm64'],
  validation: { metadata: 'passed', build: 'passed' },
}

test('catalog searches names, summaries, authors and repositories and combines categories', () => {
  for (const query of [
    'PLUGIN',
    ' deployment ',
    'Example team',
    'github.com/example',
  ]) {
    expect(filterRepositoryCatalog([plugin], query, 'Development')).toEqual([
      plugin,
    ])
  }
  expect(filterRepositoryCatalog([plugin], '', 'Security')).toEqual([])
  expect(filterRepositoryCatalog([plugin], 'missing', '')).toEqual([])
})

function renderCatalog(canInstall: boolean, available = true) {
  const client = new QueryClient()
  client.setQueryData(['repository-plugin-catalog'], {
    available,
    source:
      'https://raw.githubusercontent.com/gotempsh/plugins/main/registry/catalog.json',
    platform: 'darwin-arm64',
    plugins: available ? [plugin] : [],
    reason: available ? undefined : 'GitHub returned HTTP 404.',
  })
  try {
    return renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <RepositoryCatalog
          canInstall={canInstall}
          disabled={false}
          installedNames={[]}
          onSelect={() => {}}
        />
      </QueryClientProvider>
    )
  } finally {
    client.clear()
  }
}

test('catalog shows exact commit source link and explicit review action for admins', () => {
  const html = renderCatalog(true)
  expect(html).toContain(
    `${plugin.repository}/tree/${plugin.commit}/plugins/demo`
  )
  expect(html).toContain('Review and install')
  expect(html).toContain('not a security audit')
  expect(html).toContain('darwin-arm64')
})

test('non-admins can browse without install controls', () => {
  const html = renderCatalog(false)
  expect(html).toContain('Example plugin')
  expect(html).not.toContain('Review and install')
  expect(html).toContain('system administrator')
})

test('unavailable catalog is visible and points to a concrete recovery path', () => {
  const html = renderCatalog(true, false)
  expect(html).toContain('GitHub catalog unavailable')
  expect(html).toContain('GitHub returned HTTP 404.')
  expect(html).toContain('Refresh catalog')
  expect(html).toContain('View catalog on GitHub')
})
