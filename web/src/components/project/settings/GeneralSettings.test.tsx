// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { GeneralSettings } from './GeneralSettings'

const project = {
  id: 7,
  name: 'Sample App',
  slug: 'sample-app',
  main_branch: 'main',
  source_type: 'git',
  cross_project_trace_sharing: true,
  error_source_context_enabled: false,
  serves_own_temps_api: false,
} as unknown as ProjectResponse

function render(): string {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <GeneralSettings project={project} refetch={() => undefined} />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

/** Visible text of the `<label for="id">` that points at `id`, if any. */
function labelFor(markup: string, id: string): string | undefined {
  const escaped = id.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  const match = new RegExp(
    `<label[^>]*\\bfor="${escaped}"[^>]*>([\\s\\S]*?)</label>`
  ).exec(markup)
  if (!match) return undefined
  return match[1]
    .replace(/<svg[\s\S]*?<\/svg>/g, '')
    .replace(/<[^>]+>/g, '')
    .replace(/\s+/g, ' ')
    .trim()
}

/** The tag that carries `name="..."`, as an attribute map. */
function inputNamed(
  markup: string,
  name: string
): Record<string, string> | undefined {
  const tag = new RegExp(`<input\\b[^>]*\\bname="${name}"[^>]*>`).exec(markup)
  if (!tag) return undefined
  const attrs: Record<string, string> = {}
  for (const [, key, value] of tag[0].matchAll(/([\w:-]+)="([^"]*)"/g)) {
    attrs[key] = value
  }
  return attrs
}

test('service name and slug inputs are named by their visible labels', () => {
  const markup = render()

  const nameInput = inputNamed(markup, 'name')
  const slugInput = inputNamed(markup, 'slug')
  expect(nameInput?.id).toBeTruthy()
  expect(slugInput?.id).toBeTruthy()
  expect(nameInput?.id).not.toEqual(slugInput?.id)

  // getByLabelText resolves exactly this: label[for] -> control#id.
  expect(labelFor(markup, nameInput?.id ?? '')).toBe('Service name')
  expect(labelFor(markup, slugInput?.id ?? '')).toBe('Service slug')
})

test('the settings switches are named by their labels', () => {
  const markup = render()

  expect(markup).toContain('id="cross-project-trace-sharing"')
  expect(markup).toContain('role="switch"')
  expect(labelFor(markup, 'cross-project-trace-sharing')).toBe(
    'Cross-service trace sharing'
  )
  expect(markup).toContain('id="error-source-context"')
  expect(labelFor(markup, 'error-source-context')).toBe(
    'Source code in stack traces'
  )
  expect(markup).toContain('id="serves-own-temps-api"')
  expect(labelFor(markup, 'serves-own-temps-api')).toBe(
    'App serves its own Temps API'
  )
})

test('the Temps API routing switch is off by default and says what it moves', () => {
  const markup = render()
  const tag = /<button\b[^>]*id="serves-own-temps-api"[^>]*>/.exec(markup)?.[0]
  expect(tag).toContain('aria-checked="false"')
  expect(markup).toContain('<code>/api/_temps</code>')
  expect(markup).toContain('<code>/api/otel</code>')
})

test('every switch on the page is reachable through a label or a name', () => {
  const markup = render()
  const switches = [...markup.matchAll(/<button\b[^>]*role="switch"[^>]*>/g)]
  expect(switches.length).toBeGreaterThan(0)

  for (const [tag] of switches) {
    const id = /\bid="([^"]+)"/.exec(tag)?.[1]
    const named =
      /aria-label="[^"]+"|aria-labelledby="[^"]+"/.test(tag) ||
      (id !== undefined && labelFor(markup, id) !== undefined)
    expect(named).toBe(true)
  }
})
