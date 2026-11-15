// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { matchProjectRef, type ProjectRef } from './project-refs'

const projects: ProjectRef[] = [
  { id: 1, slug: 'shop-api', name: 'Shop API' },
  { id: 2, slug: 'docs-site', name: 'Docs' },
  { id: 3, slug: 'docs', name: 'Documentation' },
  { id: 4, slug: 'blog-a', name: 'Blog' },
  { id: 5, slug: 'blog-b', name: 'blog' },
]

describe('matchProjectRef', () => {
  it('matches an exact slug', () => {
    expect(matchProjectRef('shop-api', projects)?.id).toBe(1)
  })

  it('prefers a slug over another project’s name', () => {
    expect(matchProjectRef('docs', projects)?.id).toBe(3)
  })

  it('matches a name ignoring case and surrounding space', () => {
    expect(matchProjectRef(' shop api ', projects)?.id).toBe(1)
  })

  it('does not guess between projects sharing a name', () => {
    expect(matchProjectRef('Blog', projects)).toBeNull()
  })

  it('ignores partial names, commands and empty text', () => {
    expect(matchProjectRef('shop', projects)).toBeNull()
    expect(matchProjectRef('git push origin main', projects)).toBeNull()
    expect(matchProjectRef('  ', projects)).toBeNull()
    expect(matchProjectRef('shop-api', [])).toBeNull()
  })
})
