// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import { describePermissionDenial } from '@/lib/permission-denial-display'
import { AuditLogItemRow, categorize } from './AuditLogItem'

describe('permission-denial audit presentation', () => {
  test('categorizes permission denials as authentication events', () => {
    expect(categorize('PERMISSION_DENIED')).toBe('auth')
  })

  test('categorizes SSO events as authentication, not "other"', () => {
    // A denied SSO login is a security event operators go looking for; if it
    // falls into "Other" it is effectively invisible in the audit console.
    expect(categorize('OIDC_LOGIN_DENIED')).toBe('auth')
    expect(categorize('OIDC_PROVIDER_CREATED')).toBe('auth')
  })

  test('categorizes visitor enrichment as analytics, not "other"', () => {
    expect(categorize('VISITOR_ENRICHED')).toBe('analytics')
  })

  test('renders only normalized, redacted denial metadata', () => {
    expect(
      describePermissionDenial({
        method: 'DELETE',
        route: '/projects/{project_id}',
        auth_source: 'api_key',
        attempt_count: 3,
      })
    ).toBe('Denied DELETE /projects/{project_id} for api key (3 attempts)')
  })

  test('handles missing optional denial metadata', () => {
    expect(describePermissionDenial()).toBe('Denied a request')
  })

  test('renders a permission-denial row through the component dispatch', () => {
    const rendered = renderToStaticMarkup(
      createElement(AuditLogItemRow, {
        id: 1,
        operation_type: 'PERMISSION_DENIED',
        audit_date: 0,
        data: {
          method: 'DELETE',
          route: '/projects/{project_id}',
          auth_source: 'api_key',
          attempt_count: 3,
        },
      })
    )

    expect(rendered).toContain(
      'Denied DELETE /projects/{project_id} for api key (3 attempts)'
    )
    expect(rendered).toContain('PERMISSION_DENIED')
    expect(rendered).toContain('Auth')
  })
})

/** Rows link to projects and services, so they render inside a router. */
function renderRow(props: Parameters<typeof AuditLogItemRow>[0]): string {
  return renderToStaticMarkup(
    createElement(MemoryRouter, null, createElement(AuditLogItemRow, props))
  )
}

describe('project group audit presentation', () => {
  test('categorizes project group events as Projects, not services', () => {
    expect(categorize('PROJECT_GROUP_CREATED')).toBe('group')
    expect(categorize('PROJECT_GROUP_ROUTE_REMOVED')).toBe('group')
    expect(categorize('PROJECT_CREATED')).toBe('project')
  })

  test('describes a service joining a project with a link to the project', () => {
    const rendered = renderRow({
      id: 2,
      operation_type: 'PROJECT_GROUP_PROJECT_ADDED',
      audit_date: 0,
      data: { group_id: 4, group_slug: 'commerce', project_id: 12 },
    })
    expect(rendered).toContain('Added service (ID: 12) to project')
    expect(rendered).toContain('href="/p/commerce"')
  })

  test('names shared variables but never links a deleted project', () => {
    const variable = renderRow({
      id: 3,
      operation_type: 'PROJECT_GROUP_ENV_VAR_CREATED',
      audit_date: 0,
      data: { group_id: 4, group_slug: 'commerce', variable_key: 'API_URL' },
    })
    expect(variable).toContain('Created shared variable API_URL in project')

    const deleted = renderRow({
      id: 4,
      operation_type: 'PROJECT_GROUP_DELETED',
      audit_date: 0,
      data: { group_id: 4, group_slug: 'commerce', project_ids: [1, 2] },
    })
    expect(deleted).toContain(
      'Deleted project commerce; its 2 services are now ungrouped'
    )
    expect(deleted).not.toContain('href="/p/commerce"')
  })
})
