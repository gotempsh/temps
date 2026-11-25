// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  ALERTS_URL,
  ASSISTANT_AREA,
  SERVER_AREAS,
  SERVER_ENTITIES,
  SERVER_HOME,
  SERVER_VIEWS,
  SETTINGS_AREA_GROUPS,
  areaOwnsPath,
  isProjectsView,
  isSettingsArea,
  resolveServerLocation,
} from './server-navigation'

const area = (id: string) => {
  const found = SERVER_AREAS.find((item) => item.id === id)
  if (!found) throw new Error(`area "${id}" is missing`)
  return found
}
const urlsOf = (id: string) => [
  area(id).url,
  ...(area(id).tabs ?? []).map((tab) => tab.url),
]

describe('the server-wide pages', () => {
  test('the tree holds what is owned: projects, databases, domains, servers', () => {
    expect(SERVER_HOME.url).toBe('/projects')
    expect(SERVER_ENTITIES.map((item) => [item.title, item.url])).toEqual([
      ['Databases', '/storage'],
      ['Domains', '/domains'],
      ['Servers', '/settings/nodes'],
    ])
  })

  test('data is not an entry: it is a tab of the Projects page', () => {
    expect(SERVER_VIEWS.map((item) => item.title)).toEqual([
      'Logs',
      'Errors',
      'Traces',
      'Analytics',
      'Alerts',
    ])
    for (const view of SERVER_VIEWS) {
      expect(isProjectsView(view)).toBe(true)
      expect(SERVER_ENTITIES).not.toContain(view)
    }
    expect(isProjectsView(SERVER_HOME)).toBe(true)
    expect(isProjectsView(SERVER_ENTITIES[0])).toBe(false)
    expect(isProjectsView(undefined)).toBe(false)
  })

  test('what is set up once is a page of Settings, in two groups', () => {
    expect(
      SETTINGS_AREA_GROUPS.map((group) => [
        group.label,
        group.areas.map((item) => item.title),
      ])
    ).toEqual([
      ['Platform', ['Git', 'Email', 'Status pages', 'Workflows', 'Proxy']],
      ['AI', ['Providers', 'Connect', 'Sandboxes', 'Vault']],
    ])
    for (const path of [
      '/git-providers/add',
      '/email/providers/new',
      '/status-pages/3',
      '/proxy-logs',
      '/ai-gateway/usage',
      '/settings/mcp-server',
      '/workspaces/4',
      '/access-services',
    ])
      expect(isSettingsArea(path)).toBe(true)
    for (const path of ['/projects', '/logs', '/storage', '/chat', '/account'])
      expect(isSettingsArea(path)).toBe(false)
  })

  test('has no parent that groups pages by kind, and no catch-all', () => {
    for (const item of SERVER_AREAS) {
      expect(item.title).not.toMatch(
        /^(observe|resources|configure|ai|all tools|tools|more|other|misc)$/i
      )
      for (const url of ['/observe', '/resources', '/ai', '/tools'])
        expect(areaOwnsPath(item, url)).toBe(false)
    }
  })

  test('gives every page one home: no URL or name is listed twice', () => {
    // An area's landing page is also one of its tabs; count it once.
    const distinct = SERVER_AREAS.flatMap((item) => [
      ...new Set(urlsOf(item.id)),
    ])
    expect(new Set(distinct).size).toBe(distinct.length)
    const titles = SERVER_AREAS.map((item) => item.title)
    expect(new Set(titles).size).toBe(titles.length)
    const ids = SERVER_AREAS.map((item) => item.id)
    expect(new Set(ids).size).toBe(ids.length)
  })

  test('an area with tabs opens on one of them', () => {
    for (const item of SERVER_AREAS) {
      if (!item.tabs) continue
      expect(item.tabs.length).toBeGreaterThan(1)
      expect(item.tabs.map((tab) => tab.url)).toContain(item.url)
    }
  })

  test('keeps the pages the removed tools page used to list', () => {
    const reachable = new Set(SERVER_AREAS.flatMap((item) => urlsOf(item.id)))
    for (const url of [
      '/logs',
      '/errors',
      '/traces',
      '/analytics',
      '/revenue',
      '/status-pages',
      '/storage',
      '/backups',
      '/domains',
      '/certificates',
      '/dns-providers',
      '/delivery-profiles',
      '/proxy',
      '/proxy-logs',
      '/email',
      '/workflows',
      '/git-providers',
      '/settings/nodes',
      '/monitoring/server',
      '/sandboxes',
      '/vault',
      '/chat',
      '/ai-gateway',
      '/setup/ai',
      '/settings/mcp-server',
      '/settings/notifications',
    ])
      expect(reachable.has(url)).toBe(true)
  })

  test('the assistant is no branch or settings page: Ask stands above the tree', () => {
    expect(SETTINGS_AREA_GROUPS.flatMap((group) => group.areas)).not.toContain(
      ASSISTANT_AREA
    )
    expect(SERVER_ENTITIES).not.toContain(ASSISTANT_AREA)
    expect(area('assistant')).toMatchObject({
      url: '/chat',
      featureKey: 'ai-chat',
    })
    expect(area('providers')).toMatchObject({
      url: '/ai-gateway',
      featureKey: 'ai-gateway',
    })
    expect(area('connect').url).toBe('/setup/ai')
    expect(area('sandboxes').url).toBe('/sandboxes')
    const revenue = area('analytics').tabs?.find(
      (tab) => tab.url === '/revenue'
    )
    expect(revenue?.featureKey).toBe('revenue-tracking')
    // Retired pages stay retired.
    for (const url of ['/ai-workflows', '/skills', '/mcp-servers'])
      expect(SERVER_AREAS.some((item) => urlsOf(item.id).includes(url))).toBe(
        false
      )
  })

  test('every branch of the tree says what to add while it is empty', () => {
    for (const item of [SERVER_HOME, ...SERVER_ENTITIES]) {
      expect(item.add?.href.startsWith('/')).toBe(true)
      // A button label: a verb and a short noun.
      expect(item.add?.label.split(' ').length).toBeLessThanOrEqual(3)
    }
  })
})

describe('resolveServerLocation', () => {
  test('a tab URL is its area and the tab', () => {
    const location = resolveServerLocation('/backups')
    expect(location.area?.id).toBe('databases')
    expect(location.tab?.title).toBe('Backups')
  })

  test('a record or a form below an area belongs to it, as no tab', () => {
    for (const path of ['/storage/create', '/storage/12', '/backups/s3/3']) {
      const location = resolveServerLocation(path)
      expect(location.area?.id).toBe('databases')
      expect(location.tab).toBeUndefined()
    }
    expect(resolveServerLocation('/p/storefront/logs').area?.id).toBe(
      'projects'
    )
  })

  test('the longest prefix wins where areas share a path', () => {
    expect(resolveServerLocation('/monitoring/server').area?.id).toBe('servers')
    expect(resolveServerLocation('/monitoring/rules').area?.id).toBe('alerts')
    expect(resolveServerLocation(ALERTS_URL).tab?.title).toBe('Alarms')
    expect(resolveServerLocation('/settings/notifications/new').area?.id).toBe(
      'alerts'
    )
    expect(resolveServerLocation('/settings/nodes/4').area?.id).toBe('servers')
    expect(resolveServerLocation('/ai-gateway/usage').tab?.title).toBe('Usage')
    expect(resolveServerLocation('/settings/mcp-server').area?.id).toBe(
      'connect'
    )
  })

  test('does not match a similar prefix or a page outside the menu', () => {
    expect(resolveServerLocation('/emails')).toEqual({})
    expect(resolveServerLocation('/settings/users')).toEqual({})
    expect(resolveServerLocation('/account')).toEqual({})
    expect(areaOwnsPath(area('alerts'), '/monitoring/server')).toBe(false)
    expect(areaOwnsPath(area('email'), '/email/providers/new')).toBe(true)
  })
})
