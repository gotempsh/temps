// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  PROJECT_NAV_GROUPS,
  PROJECT_NAV_SECTIONS,
  projectPageWidth,
  projectTabNamesPage,
  projectNavGroupsFor,
  resolveProjectLocation,
} from './project-navigation'

describe('resolveProjectLocation', () => {
  test('maps each section landing page to that section', () => {
    for (const section of PROJECT_NAV_SECTIONS) {
      expect(resolveProjectLocation(section.tabs[0].url).section.id).toBe(
        section.id
      )
    }
  })

  test('prefers the longest prefix across sections', () => {
    expect(resolveProjectLocation('errors/alert-rules/new').tab.title).toBe(
      'Alerts'
    )
    expect(resolveProjectLocation('metrics/dashboards/3').tab.title).toBe(
      'Dashboards'
    )
    expect(resolveProjectLocation('metrics').tab.title).toBe('Metrics')
    expect(resolveProjectLocation('metrics/alerts/new').tab.title).toBe(
      'Alerts'
    )
    const feedback = resolveProjectLocation('errors/feedback/12')
    expect(feedback.section.id).toBe('errors')
    expect(feedback.tab.title).toBe('Feedback')
    expect(resolveProjectLocation('errors/42').tab.title).toBe('Issues')
  })

  test('keeps detail pages inside their tab', () => {
    expect(resolveProjectLocation('deployments/9').section.id).toBe(
      'deployments'
    )
    expect(
      resolveProjectLocation('environment-variables/3/checks').section.id
    ).toBe('variables')
    expect(resolveProjectLocation('security/scans/1').tab.title).toBe('Scans')
  })

  test('folds the old build, git and delivery settings into Build & deploy', () => {
    for (const route of [
      'settings/delivery',
      'connect-repository/connections/1',
    ]) {
      expect(resolveProjectLocation(route).section.id).toBe('build')
    }
  })

  test('does not match a sibling that only shares a string prefix', () => {
    expect(resolveProjectLocation('settings/securityx').section.id).toBe(
      'general'
    )
  })

  test('falls back to the overview for an unknown path', () => {
    expect(resolveProjectLocation('nope').section.id).toBe('overview')
    expect(resolveProjectLocation('').section.id).toBe('overview')
  })
})

describe('projectNavGroupsFor', () => {
  const titles = (sourceType: string) =>
    projectNavGroupsFor({ source_type: sourceType }).flatMap((group) =>
      group.sections.map(
        (section) =>
          `${section.title}: ${section.tabs.map((tab) => tab.title).join(', ')}`
      )
    )

  test('a hosted service gets the whole menu', () => {
    expect(projectNavGroupsFor({ source_type: 'git' })).toBe(PROJECT_NAV_GROUPS)
  })

  test('a monitoring-only project keeps only the pages it registers', () => {
    const menu = titles('external')
    expect(menu).toContain('Overview: Overview')
    expect(menu).toContain('General: General')
    // Hosting sections disappear entirely, not as empty headings.
    expect(menu.some((entry) => entry.startsWith('Deployments'))).toBe(false)
    expect(menu.some((entry) => entry.startsWith('Build & deploy'))).toBe(false)
    // A section keeps the tabs a monitoring project has.
    const monitoring = menu.find((entry) => entry.startsWith('Monitoring'))
    expect(monitoring).toContain('Uptime')
    expect(monitoring).toContain('Check-ins')
    // An external project runs no jobs of its own; it only reports check-ins.
    expect(monitoring).not.toContain('Cron jobs')
  })

  test("a monitoring-only project's integrations page highlights Overview", () => {
    expect(resolveProjectLocation('integrations').section.id).toBe('overview')
  })
})

describe('projectTabNamesPage', () => {
  const names = (route: string) =>
    projectTabNamesPage(route, resolveProjectLocation(route))

  test("a tab's own page is named by the tab strip", () => {
    expect(names('metrics/explore')).toBe(true)
    expect(names('metrics')).toBe(true)
    expect(names('errors')).toBe(true)
    expect(names('analytics/visitors')).toBe(true)
  })

  test('a record below a tab keeps its own title', () => {
    expect(names('errors/42')).toBe(false)
    expect(names('metrics/dashboards/3')).toBe(false)
    expect(names('security/scans/1')).toBe(false)
  })

  test('a section without a tab strip keeps its title', () => {
    expect(names('deployments')).toBe(false)
    expect(names('domains')).toBe(false)
    expect(names('settings/general')).toBe(false)
  })
})

describe('projectPageWidth', () => {
  const widthOf = (route: string, search = '') =>
    projectPageWidth(resolveProjectLocation(route), search)

  test('Configure forms are narrow, its tables use the full width', () => {
    const configure = PROJECT_NAV_GROUPS.find((g) => g.label === 'Configure')
    expect(configure).toBeDefined()
    const tables = new Set([
      'environment-variables',
      'settings/variables',
      'settings/deployment-tokens',
      'settings/edge-rules',
      'security',
    ])
    for (const section of configure?.sections ?? []) {
      for (const tab of section.tabs) {
        expect([tab.url, widthOf(tab.url)]).toEqual([
          tab.url,
          tables.has(tab.url) ? 'full' : 'form',
        ])
      }
    }
  })

  test('logs, traces and session replays use the full width', () => {
    expect(widthOf('runtime')).toBe('full')
    expect(widthOf('request-logs')).toBe('full')
    expect(widthOf('traces')).toBe('full')
    expect(widthOf('analytics/replays')).toBe('full')
    expect(widthOf('analytics/replays/abc')).toBe('full')
  })

  test('lists and dashboards use the full width', () => {
    expect(widthOf('deployments')).toBe('full')
    expect(widthOf('project')).toBe('full')
    expect(widthOf('analytics')).toBe('full')
    expect(widthOf('errors')).toBe('full')
  })

  test("an environment's settings view is a form, its dashboard is not", () => {
    expect(widthOf('environments', '?view=settings&env=production')).toBe(
      'form'
    )
    expect(widthOf('environments', '?view=containers')).toBe('full')
    expect(widthOf('environments')).toBe('full')
  })
})

describe('tab icons', () => {
  test('a section and its landing tab share an icon, so the menu and the tabs read as one', () => {
    for (const section of PROJECT_NAV_SECTIONS) {
      expect(section.tabs[0].icon).toBe(section.icon)
    }
  })

  test('no two tabs of a section share an icon', () => {
    for (const section of PROJECT_NAV_SECTIONS) {
      const icons = section.tabs.map((tab) => tab.icon)
      expect(new Set(icons).size).toBe(icons.length)
    }
  })
})
