// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { renderToStaticMarkup } from 'react-dom/server'
import { installBrowserWindow } from '@/test/dom'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { RepositoryCatalogPlugin } from '@/api/client/types.gen'
import { MemoryRouter } from 'react-router'
import {
  RepositoryCatalog,
  deployPluginPath,
  filterRepositoryCatalog,
} from './RepositoryCatalog'
import { CatalogImageActions } from './RepositoryCatalog-components'

installBrowserWindow()

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

const IMAGE = `registry.example.com/acme/example@sha256:${'a'.repeat(64)}`

function renderCatalog({
  available = true,
  added = [] as string[],
  plugins = [plugin],
  canAdd = true,
} = {}) {
  const client = new QueryClient()
  client.setQueryData(['repository-plugin-catalog'], {
    available,
    source:
      'https://raw.githubusercontent.com/gotempsh/plugins/main/registry/catalog.json',
    plugins: available ? plugins : [],
    reason: available ? undefined : 'GitHub returned HTTP 404.',
  })
  try {
    return renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <RepositoryCatalog
            addedNames={added}
            onAdd={canAdd ? () => {} : undefined}
          />
        </MemoryRouter>
      </QueryClientProvider>
    )
  } finally {
    client.clear()
  }
}

test('a listing links to its exact source and to deploying its repository as a service', () => {
  const html = renderCatalog()
  expect(html).toContain(
    `${plugin.repository}/tree/${plugin.commit}/plugins/demo`
  )
  expect(html).toContain(
    'href="/projects/new?source=git-url&amp;repo=https%3A%2F%2Fgithub.com%2Fexample%2Fplugin"'
  )
  expect(html).toContain('Copy Git link')
  // A plugin kept in a subdirectory says which one to deploy.
  expect(html).toContain('<code>plugins/demo</code>')
  expect(html).toContain('not a security audit')
  // A listing published as source only has no image to add from.
  expect(html).not.toContain('Copy command')
  expect(html).not.toContain('>Add plugin</button>')
  expect(html).not.toContain('Added</')
})

test('a listing that offers an image is added from it, not deployed from its repository', () => {
  const html = renderCatalog({ plugins: [{ ...plugin, image: IMAGE }] })
  expect(html).toContain('>Add plugin</button>')
  expect(html).toContain(`title="Add Example plugin from the image ${IMAGE}"`)
  expect(html).toContain('Copy command')
  // The source stays one click away, to review what the image was built from.
  expect(html).toContain('Review source')
  expect(html).not.toContain('Copy Git link')
  expect(html).not.toContain('/projects/new?source=git-url')
})

test('an image listing gives the command to someone who cannot add plugins, and no button', () => {
  const html = renderCatalog({
    plugins: [{ ...plugin, image: IMAGE }],
    canAdd: false,
  })
  expect(html).toContain('Copy command')
  expect(html).not.toContain('>Add plugin</button>')
})

test('adding from a listing hands over its image and the plugin name', () => {
  const chosen: unknown[] = []
  const view = render(
    <CatalogImageActions
      title="Example plugin"
      choice={{ image: IMAGE, name: 'example' }}
      added={false}
      onAdd={(choice) => chosen.push(choice)}
    />
  )
  fireEvent.click(view.getByRole('button', { name: 'Add plugin' }))
  expect(chosen).toEqual([{ image: IMAGE, name: 'example' }])
  cleanup()

  // A plugin that is already added is not offered again.
  const added = render(
    <CatalogImageActions
      title="Example plugin"
      choice={{ image: IMAGE, name: 'example' }}
      added
      onAdd={() => {}}
    />
  )
  expect(added.queryByRole('button', { name: 'Add plugin' })).toBeNull()
  cleanup()
})

test('a listing is found by the image it offers', () => {
  const listed = { ...plugin, image: IMAGE }
  expect(filterRepositoryCatalog([listed], 'acme/example', '')).toEqual([
    listed,
  ])
})

test('nothing in the catalogue installs a plugin', () => {
  const html = renderCatalog()
  expect(html).not.toContain('install')
  expect(html).not.toContain('Install')
  // The catalogue is not filtered by the server's platform any more.
  expect(html).not.toContain('Compatible with')
})

test('a plugin already added to this server is marked', () => {
  expect(renderCatalog({ added: ['example'] })).toContain('Added</')
})

test('the deploy link carries the repository as one encoded parameter', () => {
  expect(deployPluginPath('https://github.com/example/plugin?x=1&y=2')).toBe(
    '/projects/new?source=git-url&repo=https%3A%2F%2Fgithub.com%2Fexample%2Fplugin%3Fx%3D1%26y%3D2'
  )
})

test('an empty catalogue says so instead of rendering an empty list', () => {
  const html = renderCatalog({ plugins: [] })
  expect(html).toContain('No plugins are listed yet.')
  expect(html).not.toContain('No matching plugins.')
})

test('unavailable catalog is visible and points to a concrete recovery path', () => {
  const html = renderCatalog({ available: false })
  expect(html).toContain('GitHub catalog unavailable')
  expect(html).toContain('GitHub returned HTTP 404.')
  expect(html).toContain('Refresh catalog')
  expect(html).toContain('View catalog on GitHub')
})
