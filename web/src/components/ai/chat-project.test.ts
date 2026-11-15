// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { filterProjects, projectRequest } from './chat-project'

const projects = [
  { id: 1, name: 'Storefront', slug: 'storefront' },
  { id: 2, name: 'Docs site', slug: 'handbook' },
  { id: 3, name: 'API', slug: null },
]

describe('filterProjects', () => {
  test('keeps everything for an empty search', () => {
    expect(filterProjects(projects, '')).toHaveLength(3)
    expect(filterProjects(projects, '   ')).toHaveLength(3)
  })

  test('matches name or slug, ignoring case', () => {
    expect(filterProjects(projects, 'STORE').map((p) => p.id)).toEqual([1])
    expect(filterProjects(projects, 'handbook').map((p) => p.id)).toEqual([2])
    expect(filterProjects(projects, 'api').map((p) => p.id)).toEqual([3])
  })

  test('finds nothing when nothing matches', () => {
    expect(filterProjects(projects, 'zzz')).toEqual([])
  })
})

describe('projectRequest', () => {
  test('names the project only when one is chosen', () => {
    expect(projectRequest(projects[0])).toEqual({ project_id: 1 })
    expect(projectRequest(null)).toEqual({})
  })
})
