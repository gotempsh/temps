// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  projectCollectionFromApplicationProjectWrite,
  projectCollectionFromTool,
} from './tool-result-presentation'

const result = JSON.stringify({
  operation: 'get_projects',
  status: 200,
  data: {
    projects: [
      {
        id: 7,
        name: 'Storefront',
        slug: 'storefront',
        repo_owner: 'temps-sh',
        repo_name: 'storefront',
        preset: 'nextjs',
      },
    ],
    total: 1,
    page: 1,
    per_page: 10,
  },
})

describe('read tool result presentation', () => {
  test('maps get_projects receipts to the native project collection', () => {
    expect(
      projectCollectionFromTool({
        id: 'projects-1',
        name: 'mcp__temps-chat__temps',
        arguments: JSON.stringify({ command: 'projects get_projects' }),
        result,
      })
    ).toEqual({
      items: [
        {
          id: 7,
          name: 'Storefront',
          slug: 'storefront',
          repoOwner: 'temps-sh',
          repoName: 'storefront',
          preset: 'nextjs',
        },
      ],
      total: 1,
      page: 1,
      perPage: 10,
    })
  })

  test('does not let an unrelated or failed result select the component', () => {
    expect(
      projectCollectionFromTool({
        id: 'projects-2',
        name: 'temps',
        arguments: JSON.stringify({ command: 'projects get_project --id 7' }),
        result,
      })
    ).toBeNull()
    expect(
      projectCollectionFromTool({
        id: 'projects-3',
        name: 'temps',
        arguments: JSON.stringify({ command: 'projects get_projects' }),
        result: JSON.stringify({
          operation: 'get_projects',
          status: 403,
          data: {},
        }),
      })
    ).toBeNull()
  })
})

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
