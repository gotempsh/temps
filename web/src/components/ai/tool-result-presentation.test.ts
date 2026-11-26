// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { projectCollectionFromApplicationProjectWrite } from './tool-result-presentation'

describe('application project write presentation', () => {
  test('renders the committed topology only after the composite operation executes', () => {
    expect(
      projectCollectionFromApplicationProjectWrite({
        id: 'write-project-1',
        name: 'mcp__temps-chat__temps_write',
        arguments: JSON.stringify({
          command: 'create_application_project --name Storefront',
        }),
        result: JSON.stringify({
          status: 'executed',
          operation: 'create_application_project',
          result: {
            public_id: 'app_123',
            projects: [
              {
                id: 19,
                name: 'Storefront',
                slug: 'storefront',
                preset: 'nextjs',
              },
            ],
          },
        }),
      })
    ).toEqual({
      items: [
        {
          id: 19,
          name: 'Storefront',
          slug: 'storefront',
          preset: 'nextjs',
        },
      ],
      total: 1,
      page: 1,
      perPage: 1,
    })
  })

  test('does not render failed or unrelated write results as projects', () => {
    const base = {
      id: 'write-project-2',
      name: 'temps_write',
      arguments: '{}',
    }
    expect(
      projectCollectionFromApplicationProjectWrite({
        ...base,
        result: JSON.stringify({
          status: 'failed',
          operation: 'create_application_project',
          result: { projects: [] },
        }),
      })
    ).toBeNull()
    expect(
      projectCollectionFromApplicationProjectWrite({
        ...base,
        result: JSON.stringify({
          status: 'executed',
          operation: 'link_application_project',
          result: { projects: [] },
        }),
      })
    ).toBeNull()
  })
})
