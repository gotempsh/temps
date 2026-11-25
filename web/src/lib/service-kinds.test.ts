// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { GROUP_NAV_SECTIONS } from './console-scope'
import {
  sectionAvailability,
  serviceKind,
  servicesWithSection,
} from './service-kinds'

const app = {
  id: 1,
  hosting: 'hosted',
  source_type: 'git',
  project_type: 'server',
}
const site = {
  id: 2,
  hosting: 'hosted',
  source_type: 'static_files',
  project_type: 'static',
}
const monitored = {
  id: 3,
  hosting: 'external',
  source_type: 'external',
  project_type: 'server',
}

describe('serviceKind', () => {
  test('reads the kind from the member summary', () => {
    expect(serviceKind(app)).toBe('deployed')
    expect(
      serviceKind({ source_type: 'docker_image', project_type: 'service' })
    ).toBe('deployed')
    expect(serviceKind(site)).toBe('static')
    expect(serviceKind({ source_type: 'git', project_type: 'static' })).toBe(
      'static'
    )
    expect(serviceKind(monitored)).toBe('monitored')
    expect(serviceKind({ hosting: 'external', source_type: 'git' })).toBe(
      'monitored'
    )
  })
})

describe('which project pages apply', () => {
  test('a page applies when at least one service has it, and says how many', () => {
    expect(sectionAvailability([app, monitored], 'deployments')).toEqual({
      applicable: true,
      count: 1,
      total: 2,
    })
    expect(sectionAvailability([app, site, monitored], 'databases')).toEqual({
      applicable: true,
      count: 1,
      total: 3,
    })
    expect(servicesWithSection([app, site, monitored], 'domains')).toEqual([
      app,
      site,
    ])
  })

  test('monitored services alone have no deployments, domains, variables or databases', () => {
    for (const section of [
      'deployments',
      'domains',
      'variables',
      'databases',
      'backups',
    ] as const)
      expect(sectionAvailability([monitored], section).applicable).toBe(false)
    for (const section of ['logs', 'errors', 'traces', 'analytics'] as const)
      expect(sectionAvailability([monitored], section)).toEqual({
        applicable: true,
        count: 1,
        total: 1,
      })
  })

  test('static sites deploy and take domains, but use no database', () => {
    expect(sectionAvailability([site], 'deployments').applicable).toBe(true)
    expect(sectionAvailability([site], 'domains').applicable).toBe(true)
    expect(sectionAvailability([site], 'databases').applicable).toBe(false)
  })

  test('overview and settings belong to every project', () => {
    for (const services of [[], [monitored], [site]])
      for (const section of ['overview', 'settings'] as const)
        expect(sectionAvailability(services, section).applicable).toBe(true)
  })

  test('a project with no services keeps every page, so it can be explored', () => {
    for (const section of GROUP_NAV_SECTIONS)
      expect(sectionAvailability([], section.id).applicable).toBe(true)
  })
})
