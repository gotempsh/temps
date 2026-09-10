// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ThreadArtifactResponse } from '@/api/client'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { ArtifactRenderer } from './ArtifactRenderer'

function renderArtifact(artifact: ThreadArtifactResponse) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <ArtifactRenderer artifact={artifact} />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

function artifact(kind: string, payload: unknown): ThreadArtifactResponse {
  return {
    public_id: 'art_test',
    kind,
    payload,
    schema_version: 1,
    status: 'active',
    title: null,
    created_at: '2026-09-01T12:00:00Z',
    updated_at: '2026-09-01T12:00:00Z',
  }
}

describe('semantic artifact renderers', () => {
  test('renders a project collection as native project rows', () => {
    const html = renderArtifact(
      artifact('collection', {
        resource_type: 'project',
        items: [{ id: 7, name: 'Storefront', slug: 'storefront' }],
      })
    )

    expect(html).toContain('Storefront')
    expect(html).toContain('1 project')
    expect(html).not.toContain('href=')
  })

  test('never turns an unsafe model-authored project slug into a link', () => {
    const html = renderArtifact(
      artifact('collection', {
        resource_type: 'project',
        items: [{ name: 'Unsafe', slug: '../settings' }],
      })
    )

    expect(html).toContain('Unsafe')
    expect(html).not.toContain('href=')
  })

  test('renders deployment resources with the live progress card', () => {
    const html = renderArtifact(
      artifact('operation', {
        resource_type: 'deployment',
        reference: { project_id: 7, environment_id: 11 },
      })
    )

    expect(html).toContain('Deploy project')
    expect(html).toContain('Queued')
    expect(html).toContain('Environment 11')
  })

  test('renders credential requests with a real secret-settings action', () => {
    const html = renderArtifact(
      artifact('credential_request', {
        project_id: 7,
        requirements: [{ key: 'STRIPE_SECRET_KEY' }],
      })
    )

    expect(html).toContain('STRIPE_SECRET_KEY')
    expect(html).toContain('Open secret settings')
    expect(html).toContain('/projects/7/settings/environment-variables')
  })

  test('renders forms as bound form controls', () => {
    const html = renderArtifact(
      artifact('form', {
        fields: [{ name: 'region', label: 'Region', required: true }],
      })
    )

    expect(html).toContain('<form')
    expect(html).toContain('name="region"')
    expect(html).toContain('required=""')
    expect(html).toContain('Submit')
  })

  test('uses error status color instead of unconditional success green', () => {
    const html = renderArtifact(
      artifact('status', {
        rows: [{ name: 'Deploy', status: 'failed' }],
      })
    )

    expect(html).toContain('text-red-600')
    expect(html).not.toContain('stroke-success')
  })
})
