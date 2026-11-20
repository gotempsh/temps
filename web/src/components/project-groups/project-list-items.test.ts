// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ProjectGroupResponse, ProjectResponse } from '@/api/client'
import { projectListItems } from './project-list-items'

// The services endpoint sends epoch milliseconds.
const service = (id: number, deployed: string | null) =>
  ({
    id,
    slug: `s${id}`,
    name: `s${id}`,
    last_deployment: deployed ? Date.parse(deployed) : null,
  }) as unknown as ProjectResponse

const group = (id: number, deployments: (string | null)[]) =>
  ({
    id,
    slug: `g${id}`,
    name: `g${id}`,
    projects: deployments.map((last_deployment, i) => ({
      id: id * 100 + i,
      last_deployment,
    })),
  }) as unknown as ProjectGroupResponse

const keys = (items: ReturnType<typeof projectListItems>) => items.map((i) => i.key)

describe('projectListItems', () => {
  test('orders projects and services together by most recent deployment', () => {
    const items = projectListItems(
      [group(1, ['2026-09-01T00:00:00Z', '2026-09-20T00:00:00Z'])],
      [service(1, '2026-09-25T00:00:00Z'), service(2, '2026-09-10T00:00:00Z')]
    )
    expect(keys(items)).toEqual(['service-1', 'group-1', 'service-2'])
  })

  test('puts never-deployed items last, projects ahead of services', () => {
    const items = projectListItems(
      [group(1, [null]), group(2, [])],
      [service(1, null), service(2, '2026-09-10T00:00:00Z')]
    )
    expect(keys(items)).toEqual(['service-2', 'group-1', 'group-2', 'service-1'])
  })
})
