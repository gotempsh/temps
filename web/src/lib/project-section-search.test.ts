// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { projectSectionLinkMatches } from './project-section-search'

describe('project section page finder', () => {
  test('finds a page by what it shows, not only by its title', () => {
    const globe = { title: 'Globe', url: 'analytics/live', keywords: ['map'] }
    expect(projectSectionLinkMatches(globe, 'map')).toBe(true)
    expect(projectSectionLinkMatches(globe, 'funnels')).toBe(false)
  })

  test('matches titles and URLs case-insensitively', () => {
    const link = { title: 'Live globe', url: 'analytics/live' }
    expect(projectSectionLinkMatches(link, 'GLOBE')).toBe(true)
    expect(projectSectionLinkMatches(link, 'analytics/li')).toBe(true)
    expect(projectSectionLinkMatches(link, 'funnels')).toBe(false)
  })

  test('an empty or blank query matches every page', () => {
    const link = { title: 'Pages', url: 'analytics/pages' }
    expect(projectSectionLinkMatches(link, '')).toBe(true)
    expect(projectSectionLinkMatches(link, '   ')).toBe(true)
  })
})
