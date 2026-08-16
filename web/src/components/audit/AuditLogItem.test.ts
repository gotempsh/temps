import { describe, expect, test } from 'bun:test'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'

import { describePermissionDenial } from '@/lib/permission-denial-display'
import { AuditLogItemRow, categorize } from './AuditLogItem'

describe('permission-denial audit presentation', () => {
  test('categorizes permission denials as authentication events', () => {
    expect(categorize('PERMISSION_DENIED')).toBe('auth')
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
