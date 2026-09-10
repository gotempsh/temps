// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { describe, expect, test } from 'bun:test'
import {
  PROJECT_PRIMARY_ROUTES,
  PROJECT_SECTION_LINKS,
  resolveProjectPrimaryRoute,
  resolveProjectSectionLink,
} from './project-navigation'
import {
  flattenProjectTools,
  projectToolGroups,
} from '@/components/project/project-tools'

describe('flat project navigation', () => {
  test('has exactly seven sections and gives security its own home', () => {
    expect(PROJECT_PRIMARY_ROUTES).toEqual([
      'project',
      'deployments',
      'environments',
      'observe',
      'storage',
      'security',
      'settings',
    ])
    for (const path of [
      'security',
      'security/scans/1',
      'security/scans/1/vulnerabilities/2',
      'settings/security',
      'settings/access',
    ])
      expect(resolveProjectPrimaryRoute(path)).toBe('security')
  })
  test('keeps deep links in their owning section', () => {
    for (const path of [
      'analytics/pages',
      'analytics/visitors/42',
      'errors/12',
      'errors/alert-rules/new',
      'traces/abc',
      'runtime',
      'request-logs/42',
      'telemetry-logs',
      'dashboards/custom',
    ])
      expect(resolveProjectPrimaryRoute(path)).toBe('observe')
    expect(resolveProjectPrimaryRoute('deployments/99')).toBe('deployments')
    expect(resolveProjectPrimaryRoute('environments/1')).toBe('environments')
    expect(resolveProjectPrimaryRoute('services/blob')).toBe('storage')
    expect(resolveProjectPrimaryRoute('')).toBe('project')
    expect(resolveProjectPrimaryRoute('agents/detail/example')).toBe('settings')
  })
  test('selects the most specific sibling and respects legacy build query links', () => {
    expect(
      resolveProjectSectionLink('observe', 'errors/alert-rules/new', '')
    ).toBe('errors/alert-rules')
    expect(
      resolveProjectSectionLink('observe', 'analytics/visitors/42', '')
    ).toBe('analytics/visitors')
    for (const route of ['build', 'settings/build']) {
      for (const tab of ['source', 'build', 'deploy', 'previews'])
        expect(
          resolveProjectSectionLink('settings', route, `?tab=${tab}`)
        ).toBe('settings/delivery')
      expect(resolveProjectSectionLink('settings', route, '?tab=invalid')).toBe(
        'settings/delivery'
      )
    }
    expect(resolveProjectSectionLink('security', 'settings/security', '')).toBe(
      'settings/security'
    )
    expect(
      resolveProjectSectionLink(
        'settings',
        'settings/environment-variables',
        ''
      )
    ).toBe('settings/variables')
  })
  test('settings has only seven destinations', () => {
    expect(PROJECT_SECTION_LINKS.settings).toHaveLength(7)
  })
  test('every existing tool has a direct contextual destination', () => {
    for (const tool of flattenProjectTools(projectToolGroups)) {
      const [route, search] = tool.url.split('?')
      const section = resolveProjectPrimaryRoute(route)
      expect(
        resolveProjectSectionLink(section, route, search ?? '')
      ).toBeDefined()
    }
    for (const [section, links] of Object.entries(PROJECT_SECTION_LINKS))
      for (const link of links ?? [])
        expect(String(resolveProjectPrimaryRoute(link.url))).toEqual(section)
  })
})
