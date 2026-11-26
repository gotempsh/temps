// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  PROJECT_NAV_SECTIONS,
  heldSectionOf,
  projectEntityViewOf,
  projectEntityViewsFor,
  projectEnvironmentScope,
  projectLocationFor,
  projectNavSectionsFor,
  projectPageWidth,
  projectTabNamesPage,
  resolveProjectLocation,
} from './project-navigation'

describe("a service's views", () => {
  test('are one list, with no parent grouping them by kind', () => {
    expect(PROJECT_NAV_SECTIONS.map((section) => section.title)).toEqual([
      'Overview',
      'Deployments',
      'Logs',
      'Errors',
      'Variables',
      'Traces',
      'Metrics',
      'Uptime',
      'Analytics',
      'Alerts',
      'Domains',
      'Databases',
      'Security',
      'Automation',
      'Settings',
    ])
    // The former parents are not pages: their sub-paths are the overview's.
    for (const path of ['observe', 'resources', 'configure'])
      expect(resolveProjectLocation(path).section.id).toBe('overview')
  })

  test('names each concept once', () => {
    const titles = PROJECT_NAV_SECTIONS.map((section) => section.title)
    expect(new Set(titles).size).toBe(titles.length)
    // "Telemetry" used to name both the OTLP logs and the ingest settings.
    const tabs = PROJECT_NAV_SECTIONS.flatMap((section) =>
      section.tabs.flatMap((tab) => [tab, ...(tab.views ?? [])])
    ).map((tab) => tab.title)
    expect(tabs).not.toContain('Telemetry')
    expect(tabs).toContain('OpenTelemetry')
    expect(tabs).toContain('Data ingest')
  })

  test('gives every page one home', () => {
    const urls = PROJECT_NAV_SECTIONS.flatMap((section) =>
      section.tabs.flatMap((tab) =>
        (tab.views ?? [tab]).map((page) => page.url)
      )
    )
    expect(new Set(urls).size).toBe(urls.length)
  })

  test('keeps a section to a few tabs, with the rest one row down', () => {
    const tabsOf = (id: string) =>
      PROJECT_NAV_SECTIONS.find((section) => section.id === id)?.tabs.map(
        (tab) => tab.title
      )
    expect(tabsOf('errors')).toEqual([
      'Issues',
      'Feedback',
      'Releases',
      'Settings',
    ])
    expect(tabsOf('analytics')).toEqual([
      'Overview',
      'Visitors',
      'Pages',
      'Funnels',
      'Revenue',
      'Settings',
    ])
    expect(tabsOf('deployments')).toEqual([
      'Deployments',
      'Environments',
      'Build',
    ])
    for (const section of PROJECT_NAV_SECTIONS) {
      expect(section.tabs.length).toBeLessThanOrEqual(6)
      for (const tab of section.tabs) {
        if (!tab.views) continue
        // A tab that holds pages opens on the first of them.
        expect(tab.url).toBe(tab.views[0].url)
      }
    }
  })
})

describe('resolveProjectLocation', () => {
  test('maps each section landing page to that section', () => {
    for (const section of PROJECT_NAV_SECTIONS) {
      expect(resolveProjectLocation(section.tabs[0].url).section.id).toBe(
        section.id
      )
    }
  })

  test('prefers the longest prefix across sections', () => {
    const errorAlert = resolveProjectLocation('errors/alert-rules/new')
    expect(errorAlert.section.id).toBe('alerts')
    expect(errorAlert.tab.title).toBe('Error alerts')
    expect(resolveProjectLocation('metrics/dashboards/3').tab.title).toBe(
      'Dashboards'
    )
    expect(resolveProjectLocation('metrics').tab.title).toBe('Metrics')
    const metricAlert = resolveProjectLocation('metrics/alerts/new')
    expect(metricAlert.section.id).toBe('alerts')
    expect(metricAlert.tab.title).toBe('Metric alerts')
    expect(resolveProjectLocation('monitors/3').section.id).toBe('uptime')
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

  test('folds the old build, git and delivery settings into Deployments › Build', () => {
    for (const route of [
      'settings/delivery',
      'connect-repository/connections/1',
      'build-deploy',
    ]) {
      const location = resolveProjectLocation(route)
      expect(location.section.id).toBe('deployments')
      expect(location.tab.title).toBe('Build')
    }
    expect(resolveProjectLocation('environments').tab.title).toBe(
      'Environments'
    )
  })

  test('a page behind a tab resolves to the tab and its view', () => {
    const maps = resolveProjectLocation('errors/source-maps')
    expect(maps.section.id).toBe('errors')
    expect(maps.tab.title).toBe('Settings')
    expect(maps.view?.title).toBe('Source maps')
    const replay = resolveProjectLocation('analytics/replays/abc')
    expect(replay.tab.title).toBe('Visitors')
    expect(replay.view?.title).toBe('Sessions')
    expect(resolveProjectLocation('speed').tab.title).toBe('Pages')
    // A tab that is one page has no view.
    expect(resolveProjectLocation('errors').view).toBeUndefined()
  })

  test('the former Activity page keeps its place', () => {
    expect(resolveProjectLocation('activity').tab.title).toBe('Activity')
  })

  test('the environment bar follows the page, not only its section', () => {
    const scopeOf = (route: string) =>
      projectEnvironmentScope(resolveProjectLocation(route))
    expect(scopeOf('deployments')).toBe('all')
    expect(scopeOf('environments')).toBe('single')
    expect(scopeOf('build-deploy')).toBe('single')
    expect(scopeOf('errors/alert-rules')).toBe('all')
    expect(scopeOf('metrics/alerts')).toBe('single')
    expect(scopeOf('domains')).toBe('project')
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

describe('projectNavSectionsFor', () => {
  const titles = (sourceType: string) =>
    projectNavSectionsFor({ source_type: sourceType }).map(
      (section) =>
        `${section.title}: ${section.tabs.map((tab) => tab.title).join(', ')}`
    )

  test('a hosted service gets every view', () => {
    expect(projectNavSectionsFor({ source_type: 'git' })).toBe(
      PROJECT_NAV_SECTIONS
    )
  })

  test('a monitoring-only project keeps only the pages it registers', () => {
    const menu = titles('external')
    expect(menu).toContain('Overview: Overview')
    expect(menu).toContain('Settings: General, Data ingest')
    // Hosting sections disappear entirely, not as empty headings.
    expect(menu.some((entry) => entry.startsWith('Deployments'))).toBe(false)
    expect(menu.some((entry) => entry.startsWith('Variables'))).toBe(false)
    // A section keeps the tabs a monitoring project has.
    const uptime = menu.find((entry) => entry.startsWith('Uptime'))
    expect(uptime).toContain('Monitors')
    expect(uptime).toContain('Check-ins')
    // An external project runs no jobs of its own; it only reports check-ins.
    expect(uptime).not.toContain('Cron jobs')
  })

  test('a monitoring-only project has no domains or databases of its own', () => {
    const ids = projectNavSectionsFor({ source_type: 'external' }).map(
      (section) => section.id
    )
    expect(ids).not.toContain('domains')
    expect(ids).not.toContain('databases')
    expect(ids).toContain('traces')
    expect(ids).toContain('general')
  })

  test('a tab left with one page is that page, with no second row', () => {
    const external = { source_type: 'external' }
    const analytics = projectNavSectionsFor(external).find(
      (section) => section.id === 'analytics'
    )
    const pages = analytics?.tabs.find((tab) => tab.title === 'Pages')
    // `speed` is registered; `analytics/*` too, so Pages keeps its views.
    expect(pages?.views?.map((view) => view.title)).toContain('Web performance')
    const location = projectLocationFor(external, 'monitors')
    expect(location.section.tabs.map((tab) => tab.title)).toEqual([
      'Monitors',
      'Check-ins',
    ])
    expect(location.tab.title).toBe('Monitors')
  })

  test("a monitoring-only project's integrations page highlights Overview", () => {
    expect(resolveProjectLocation('integrations').section.id).toBe('overview')
  })
})

describe('projectEntityViewsFor', () => {
  const hosted = projectEntityViewsFor({ source_type: 'git' })

  test('the views of a service: the everyday seven, Settings last', () => {
    expect(hosted.map((view) => view.title)).toEqual([
      'Overview',
      'Deployments',
      'Logs',
      'Errors',
      'Variables',
      'Analytics',
      'Settings',
    ])
  })

  test('an occasional section is a tab of the view answering its question', () => {
    const of = (id: string) => hosted.find((view) => view.id === id)
    expect(of('overview')?.sections.map((section) => section.id)).toEqual([
      'overview',
      'metrics',
      'uptime',
      'alerts',
      'domains',
      'databases',
    ])
    expect(
      of('overview')?.tabs.map((tab) => [
        tab.title,
        tab.views?.map((page) => page.title),
      ])
    ).toEqual([
      ['Overview', undefined],
      ['Activity', undefined],
      ['Metrics', ['Metrics', 'Dashboards']],
      ['Uptime', ['Monitors', 'Cron jobs', 'Check-ins', 'Workflows']],
      ['Alerts', ['Error alerts', 'Metric alerts']],
      ['Domains', undefined],
      ['Databases', ['Databases', 'Key-value', 'Blob']],
    ])
    expect(of('logs')?.tabs.map((tab) => tab.title)).toEqual([
      'Application',
      'Requests',
      'OpenTelemetry',
      'Traces',
    ])
  })

  test('the tour finds a held section by its tab', () => {
    const overview = hosted[0]
    expect(
      overview.tabs.map((tab) => heldSectionOf(overview, tab) ?? null)
    ).toEqual([
      null,
      null,
      'metrics',
      'uptime',
      'alerts',
      'domains',
      'databases',
    ])
  })

  test('Settings holds what is configured once, its own pages first', () => {
    const settings = hosted[hosted.length - 1]
    expect(settings.id).toBe('general')
    expect(settings.sections.map((section) => section.id)).toEqual([
      'general',
      'access',
      'automation',
    ])
    expect(settings.tabs.map((tab) => tab.title)).toEqual([
      'General',
      'Data ingest',
      'Protection',
      'Edge rules',
      'Access',
      'Scans',
      'Automation',
      'Integrations',
    ])
  })

  test('every section is behind exactly one tab', () => {
    const behind = hosted.flatMap((view) =>
      view.sections.map((section) => section.id)
    )
    expect([...behind].sort()).toEqual(
      PROJECT_NAV_SECTIONS.map((section) => section.id).sort()
    )
  })

  test('a monitoring-only project keeps a Settings tab with the pages it has', () => {
    const views = projectEntityViewsFor({ source_type: 'external' })
    const settings = views[views.length - 1]
    expect(settings.title).toBe('Settings')
    expect(settings.tabs.map((tab) => tab.title)).toEqual([
      'General',
      'Data ingest',
    ])
    expect(views.map((view) => view.title)).not.toContain('Deployments')
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
    // A view's own page is named by the second row.
    expect(names('errors/source-maps')).toBe(true)
    expect(names('analytics/replays')).toBe(true)
  })

  test('a record below a tab keeps its own title', () => {
    expect(names('errors/42')).toBe(false)
    expect(names('metrics/dashboards/3')).toBe(false)
    expect(names('security/scans/1')).toBe(false)
    expect(names('analytics/replays/abc')).toBe(false)
  })

  test("a view with one page is named by the service's tabs", () => {
    expect(names('domains')).toBe(true)
    expect(names('traces')).toBe(true)
  })

  test('the Overview names the service, whatever its tab says', () => {
    expect(names('project')).toBe(false)
  })
})

describe('projectEntityViewOf', () => {
  const viewOf = (route: string, sourceType = 'git') => {
    const project = { source_type: sourceType }
    return projectEntityViewOf(project, projectLocationFor(project, route))
  }

  test('a page belongs to the view whose section holds it', () => {
    expect(viewOf('environments')?.title).toBe('Deployments')
    expect(viewOf('errors/42')?.title).toBe('Errors')
    expect(viewOf('metrics/dashboards/3')?.title).toBe('Overview')
    expect(viewOf('errors/alert-rules')?.title).toBe('Overview')
    expect(viewOf('cron-jobs')?.title).toBe('Overview')
    expect(viewOf('traces/abc')?.title).toBe('Logs')
    expect(viewOf('traces', 'external')?.title).toBe('Logs')
  })

  test('security, automation and the service settings are one Settings view', () => {
    for (const route of [
      'settings/general',
      'settings/security',
      'settings/automation',
      'security',
    ])
      expect(viewOf(route)?.id).toBe('general')
    expect(viewOf('settings/security')?.tabs.map((tab) => tab.title)).toEqual([
      'General',
      'Data ingest',
      'Protection',
      'Edge rules',
      'Access',
      'Scans',
      'Automation',
      'Integrations',
    ])
  })
})

describe('projectPageWidth', () => {
  const widthOf = (route: string, search = '') =>
    projectPageWidth(resolveProjectLocation(route), search)

  test('Settings forms are narrow, its tables use the full width', () => {
    const settings = projectEntityViewsFor({ source_type: 'git' }).find(
      (view) => view.title === 'Settings'
    )
    expect(settings).toBeDefined()
    const tables = new Set([
      'environment-variables',
      'settings/variables',
      'settings/deployment-tokens',
      'settings/edge-rules',
      'security',
    ])
    for (const section of settings?.sections ?? []) {
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
