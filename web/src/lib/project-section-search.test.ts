// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { PROJECT_SECTION_LINKS } from './project-navigation'
import { projectSectionLinkMatches } from './project-section-search'

const analyticsMatches = (search: string) =>
  (PROJECT_SECTION_LINKS.analytics ?? [])
    .filter((link) => projectSectionLinkMatches(link, search))
    .map((link) => link.url)

describe('project section page finder', () => {
  test('finds the live and journey analytics views by what they show', () => {
    expect(analyticsMatches('live')).toEqual(
      expect.arrayContaining(['analytics/live-visitors', 'analytics/live'])
    )
    expect(analyticsMatches('globe')).toEqual(['analytics/live'])
    expect(analyticsMatches('journey')).toEqual(['analytics/journey'])
    expect(analyticsMatches('flow')).toContain('analytics/journey')
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
