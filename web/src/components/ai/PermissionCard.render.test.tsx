// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import { PermissionCard, type PermissionRequest } from './PermissionCard'

function renderPermission(input: Record<string, unknown>) {
  const permission: PermissionRequest = {
    id: 'permission-1',
    kind: 'tool_approval',
    tool_name: 'temps_write',
    input,
  }
  return renderToStaticMarkup(
    <MemoryRouter>
      <PermissionCard
        conversationBasePath="/api/ai/conversations"
        conversationPublicId="conversation-1"
        permission={permission}
      />
    </MemoryRouter>
  )
}

describe('PermissionCard rich platform writes', () => {
  test('renders a native database creation approval instead of raw JSON', () => {
    const markup = renderPermission({
      operation: 'create_service',
      method: 'POST',
      summary: 'Create a PostgreSQL database',
      parameters: {
        name: 'app-db',
        service_type: 'postgres',
        version: '18',
        project_id: 42,
        parameters: {
          database: 'app',
          username: 'postgres',
          password: '********',
        },
      },
    })

    expect(markup).toContain('Create PostgreSQL')
    expect(markup).toContain('aria-label="PostgreSQL logo"')
    expect(markup).toContain('Target project')
    expect(markup).toContain('Project 42')
    expect(markup).toContain('Approve &amp; run')
    expect(markup).not.toContain('password')
    expect(markup).not.toContain('parameters:')
  })

  test('renders a native existing-database link approval', () => {
    const markup = renderPermission({
      operation: 'link_service_to_project',
      method: 'POST',
      parameters: { id: 7, project_id: 42 },
    })

    expect(markup).toContain('Link existing database')
    expect(markup).toContain('Service 7')
    expect(markup).toContain('Project 42')
    expect(markup).toContain('refresh workspace networking')
    expect(markup).not.toContain('parameters:')
  })
})
