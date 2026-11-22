// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ProjectResponse,
  ServiceTemplateInstance,
  ServiceTemplateInstanceResponse,
} from '@/api/client'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { describe, expect, test } from 'bun:test'
import {
  ServiceTemplateCatalogUnavailable,
  ServiceTemplateUpgradeCard,
} from './ServiceTemplateRuntimeCard'

describe('ServiceTemplateCatalogUnavailable', () => {
  test('keeps the applied runtime actionable when catalog upgrades are unavailable', () => {
    const markup = renderToStaticMarkup(
      <ServiceTemplateCatalogUnavailable message="The active catalog could not be read." />
    )

    expect(markup).toContain('Template updates unavailable')
    expect(markup).toContain('The active catalog could not be read.')
    expect(markup).toContain('saved runtime remains deployable and editable')
  })
})

function release(version: string): ServiceTemplateInstance {
  return {
    schema_version: '1',
    slug: 'acme-app',
    version,
    service: 'web',
    template: { kind: 'git', name: 'Acme app' },
  } as unknown as ServiceTemplateInstance
}

function instanceResponse(
  overrides: Partial<ServiceTemplateInstanceResponse>
): ServiceTemplateInstanceResponse {
  return {
    project_id: 7,
    applied: release('1.0.0'),
    latest: release('2.0.0'),
    catalog_error: null,
    upgrade_available: false,
    catalog_drift: false,
    changes: [],
    required_configuration: [],
    missing_services: [],
    ...overrides,
  }
}

function renderUpgradeCard(instance: ServiceTemplateInstanceResponse): string {
  return renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <MemoryRouter>
        <ServiceTemplateUpgradeCard
          project={{ id: 7, slug: 'demo' } as unknown as ProjectResponse}
          instance={instance}
          onUpgraded={async () => {}}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

describe('ServiceTemplateUpgradeCard', () => {
  test('explains a newer release that cannot be applied to this service', () => {
    const markup = renderUpgradeCard(
      instanceResponse({
        catalog_error:
          'Project 7 runs a single service of acme-app, but release acme-app@2.0.0 has 3 services. Deploy Acme app as a whole app from the template catalog instead.',
      })
    )

    expect(markup).toContain('Newer release cannot be applied')
    expect(markup).toContain('2.0.0')
    expect(markup).toContain('Deploy Acme app as a whole app')
    expect(markup).toContain('template=acme-app')
  })

  test('renders the explanation when the catalog has no latest release', () => {
    const markup = renderUpgradeCard(
      instanceResponse({
        latest: null,
        catalog_error: 'This service family is no longer present.',
      })
    )

    expect(markup).toContain('Template updates unavailable')
    expect(markup).toContain('This service family is no longer present.')
  })

  test('renders nothing when there is no upgrade and no explanation', () => {
    expect(renderUpgradeCard(instanceResponse({}))).toBe('')
  })

  test('keeps the explanation visible alongside an available upgrade', () => {
    const markup = renderUpgradeCard(
      instanceResponse({
        upgrade_available: true,
        catalog_error: 'Something the operator should know.',
      })
    )

    expect(markup).toContain('Update available')
    expect(markup).toContain('Something the operator should know.')
  })
})
