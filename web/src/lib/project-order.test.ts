// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { projectListItems } from '@/components/project-groups/project-list-items'
import type { ProjectGroupResponse, ProjectResponse } from '@/api/client'
import { pickProjects, projectNodes } from './entity-tree'
import { deployedAt, latestDeployment, orderProjectList } from './project-order'

type Svc = { id: number; last_deployment?: string | number | null }
type Grp = { id: number; projects: Svc[] }

const at = (iso: string) => Date.parse(iso)
const order = (groups: Grp[], services: Svc[]) =>
  orderProjectList(groups, services, {
    group: (group) => `g${group.id}`,
    service: (service) => `s${service.id}`,
  })

describe('deployedAt', () => {
  test('reads epoch ms and ISO strings alike', () => {
    expect(deployedAt(at('2026-09-01T00:00:00Z'))).toBe(
      at('2026-09-01T00:00:00Z')
    )
    expect(deployedAt('2026-09-01T00:00:00Z')).toBe(at('2026-09-01T00:00:00Z'))
  })

  test('never deployed, or an unreadable time, sorts last', () => {
    expect(deployedAt(null)).toBe(-Infinity)
    expect(deployedAt(undefined)).toBe(-Infinity)
    expect(deployedAt('not a date')).toBe(-Infinity)
  })
})

describe('latestDeployment', () => {
  test('is the newest of the services, or -Infinity with none deployed', () => {
    expect(
      latestDeployment([
        { last_deployment: '2026-09-01T00:00:00Z' },
        { last_deployment: null },
        { last_deployment: at('2026-09-20T00:00:00Z') },
      ])
    ).toBe(at('2026-09-20T00:00:00Z'))
    expect(latestDeployment([])).toBe(-Infinity)
    expect(latestDeployment([{ last_deployment: null }])).toBe(-Infinity)
  })
})

describe('orderProjectList', () => {
  test('mixes projects and services by most recent deployment', () => {
    expect(
      order(
        [
          {
            id: 1,
            projects: [
              { id: 10, last_deployment: '2026-09-01T00:00:00Z' },
              { id: 11, last_deployment: '2026-09-20T00:00:00Z' },
            ],
          },
        ],
        [
          { id: 2, last_deployment: at('2026-09-25T00:00:00Z') },
          { id: 3, last_deployment: at('2026-09-10T00:00:00Z') },
        ]
      )
    ).toEqual(['s2', 'g1', 's3'])
  })

  test('a project is not listed first just for being a project', () => {
    // The reported symptom: the sidebar put every project ahead of services.
    expect(
      order(
        [
          {
            id: 1,
            projects: [{ id: 10, last_deployment: '2026-01-01T00:00:00Z' }],
          },
        ],
        [{ id: 2, last_deployment: '2026-09-01T00:00:00Z' }]
      )
    ).toEqual(['s2', 'g1'])
  })

  test('ties keep projects ahead of services, then the incoming order', () => {
    const same = '2026-09-01T00:00:00Z'
    expect(
      order(
        [
          { id: 1, projects: [{ id: 10, last_deployment: same }] },
          { id: 2, projects: [{ id: 20, last_deployment: same }] },
        ],
        [
          { id: 3, last_deployment: at(same) },
          { id: 4, last_deployment: same },
        ]
      )
    ).toEqual(['g1', 'g2', 's3', 's4'])
  })

  test('never-deployed items go last: empty projects too, projects first', () => {
    expect(
      order(
        [
          { id: 1, projects: [] },
          { id: 2, projects: [{ id: 20, last_deployment: null }] },
        ],
        [
          { id: 3 },
          { id: 4, last_deployment: '2026-09-01T00:00:00Z' },
          { id: 5, last_deployment: null },
        ]
      )
    ).toEqual(['s4', 'g1', 'g2', 's3', 's5'])
  })

  test('does not reorder its input', () => {
    const services = [
      { id: 1, last_deployment: null },
      { id: 2, last_deployment: '2026-09-01T00:00:00Z' },
    ]
    order([], services)
    expect(services.map((s) => s.id)).toEqual([1, 2])
  })
})

describe('the sidebar and the Projects page agree', () => {
  const groups = [
    {
      id: 1,
      name: 'Shop',
      slug: 'shop',
      projects: [
        {
          id: 10,
          name: 'web',
          slug: 'web',
          last_deployment: '2026-09-05T00:00:00Z',
        },
      ],
    },
    { id: 2, name: 'Empty', slug: 'empty', projects: [] },
  ]
  const standalone = [
    {
      id: 20,
      name: 'blog',
      slug: 'blog',
      last_deployment: at('2026-09-10T00:00:00Z'),
    },
    { id: 21, name: 'docs', slug: 'docs', last_deployment: null },
    {
      id: 22,
      name: 'api',
      slug: 'api',
      last_deployment: at('2026-09-01T00:00:00Z'),
    },
  ]
  const pageOrder = projectListItems(
    groups as unknown as ProjectGroupResponse[],
    standalone as unknown as ProjectResponse[]
  ).map((item) =>
    item.kind === 'group'
      ? `project:${item.group.id}`
      : `service:${item.project.id}`
  )

  test('the tree lists every row in the page order', () => {
    // The tree reads every service, grouped ones included; it drops those
    // the page lists inside their project's card.
    const nodes = projectNodes(groups, [...standalone, groups[0].projects[0]])
    expect(nodes.map((node) => node.key)).toEqual(pageOrder)
    expect(pageOrder).toEqual([
      'service:20',
      'project:1',
      'service:22',
      'project:2',
      'service:21',
    ])
  })

  test('a capped tree keeps the page order, trouble included', () => {
    const nodes = projectNodes(groups, standalone)
    const picked = pickProjects(nodes, 3, {
      isCurrent: () => false,
      // In trouble, but deployed long ago: picked, not moved to the top.
      inTrouble: (node) => node.key === 'service:21',
      recent: [],
    })
    expect(picked.shown.map((node) => node.key)).toEqual([
      'service:20',
      'project:1',
      'service:21',
    ])
    expect(picked.more).toBe(2)
  })
})
