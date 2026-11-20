// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import type {
  ApplicationProjectResponse,
  ChatResource,
} from '@/api/client/types.gen'
import { chatProjects, projectHref } from './chat-projects'

function linked(
  id: number,
  slug: string,
  is_primary = false
): ApplicationProjectResponse {
  return {
    id,
    slug,
    name: slug,
    is_primary,
    automatic_deploy: false,
    environments: [],
    is_private: false,
    main_branch: 'main',
  }
}

function made(id: string, slug: string | null): ChatResource {
  return {
    id,
    kind: 'project',
    name: slug ?? id,
    slug,
    created_at: '2026-10-20T10:00:00Z',
  }
}

describe('chatProjects', () => {
  it('lists the primary project first', () => {
    const projects = chatProjects(
      [linked(1, 'api'), linked(2, 'web', true)],
      []
    )
    expect(projects.map((p) => p.slug)).toEqual(['web', 'api'])
  })

  it('adds projects the chat made that the application does not list', () => {
    const projects = chatProjects(
      [linked(1, 'web', true)],
      [made('1', 'web'), made('3', 'worker')]
    )
    expect(projects.map((p) => p.id)).toEqual([1, 3])
  })

  it('shows what a chat on its own made', () => {
    const projects = chatProjects([], [made('4', 'shop')])
    expect(projects).toEqual([{ id: 4, slug: 'shop', name: 'shop' }])
  })

  it('leaves out pieces that are not projects or cannot be opened', () => {
    const service: ChatResource = {
      ...made('5', 'db'),
      kind: 'service',
    }
    expect(chatProjects([], [service, made('6', null)])).toEqual([])
  })

  it('links to the project page by slug', () => {
    expect(projectHref({ id: 1, slug: 'web', name: 'Web' })).toBe(
      '/projects/web'
    )
  })
})
