// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  capRows,
  databaseState,
  domainState,
  locateInTree,
  projectNodes,
  serverState,
  serviceState,
  worstState,
} from './entity-tree'

describe('locateInTree', () => {
  test('a project and a service sit under Projects', () => {
    expect(locateInTree('/p/shop/logs')).toEqual({
      branch: 'projects',
      groupSlug: 'shop',
    })
    expect(locateInTree('/projects/web/deployments')).toEqual({
      branch: 'projects',
      serviceSlug: 'web',
    })
  })

  test('the data of every service belongs to Projects, not to a branch of its own', () => {
    for (const path of [
      '/projects',
      '/logs',
      '/errors',
      '/traces',
      '/analytics',
      '/monitoring/alarms',
    ])
      expect(locateInTree(path).branch).toBe('projects')
  })

  test('a record names its branch and its id', () => {
    expect(locateInTree('/storage/12')).toEqual({
      branch: 'databases',
      recordId: 12,
    })
    expect(locateInTree('/domains/4')).toEqual({
      branch: 'domains',
      recordId: 4,
    })
    expect(locateInTree('/settings/nodes/7/containers')).toEqual({
      branch: 'servers',
      recordId: 7,
    })
  })

  test('a list page names its branch without a record', () => {
    expect(locateInTree('/storage')).toEqual({ branch: 'databases' })
    expect(locateInTree('/backups')).toEqual({ branch: 'databases' })
    expect(locateInTree('/certificates')).toEqual({ branch: 'domains' })
    expect(locateInTree('/settings/nodes')).toEqual({ branch: 'servers' })
    expect(locateInTree('/monitoring/server')).toEqual({ branch: 'servers' })
  })

  test('a form of a branch is not mistaken for a record', () => {
    expect(locateInTree('/storage/create')).toEqual({ branch: 'databases' })
    expect(locateInTree('/domains/add')).toEqual({ branch: 'domains' })
  })

  test('a page outside the tree belongs to no branch', () => {
    expect(locateInTree('/settings/users').branch).toBeUndefined()
    expect(locateInTree('/nowhere').branch).toBeUndefined()
  })
})

describe('states', () => {
  test('a service nothing checks has no state: it is not called healthy', () => {
    expect(serviceState('no_monitors')).toBeUndefined()
    expect(serviceState(undefined)).toBeUndefined()
    expect(serviceState('operational')).toEqual({
      tone: 'ok',
      label: 'Healthy',
    })
    expect(serviceState('down')).toEqual({ tone: 'bad', label: 'Down' })
    expect(serviceState('not_deployed')?.tone).toBe('idle')
  })

  test('databases, domains and servers each say theirs in words', () => {
    expect(databaseState('running')?.label).toBe('Running')
    expect(databaseState('failed')?.tone).toBe('bad')
    expect(databaseState('creating')?.tone).toBe('warn')
    expect(databaseState('stopped')?.tone).toBe('idle')
    expect(domainState('active_renewal_failed')).toEqual({
      tone: 'warn',
      label: 'Renewal failed',
    })
    expect(domainState('pending_dns')?.tone).toBe('warn')
    expect(serverState('offline')).toEqual({ tone: 'bad', label: 'Offline' })
    expect(serverState('drained')?.tone).toBe('warn')
  })

  test('an unrecognised status claims nothing', () => {
    expect(databaseState('teleporting')).toBeUndefined()
    expect(domainState('teleporting')).toBeUndefined()
    expect(serverState('teleporting')).toBeUndefined()
  })
})

describe('worstState', () => {
  const ok = serviceState('operational')
  const down = serviceState('down')
  const degraded = serviceState('degraded')

  test('nothing known says nothing', () => {
    expect(worstState([])).toBeUndefined()
    expect(worstState([undefined, undefined])).toBeUndefined()
  })

  test('the worst wins, and unknown services do not count', () => {
    expect(worstState([ok, undefined, degraded])).toEqual({
      tone: 'warn',
      label: 'Degraded',
    })
    expect(worstState([ok, down, degraded])).toEqual({
      tone: 'bad',
      label: 'Down',
    })
    expect(worstState([ok, ok])).toEqual({ tone: 'ok', label: 'Healthy' })
  })

  test('several in the same bad state are counted', () => {
    expect(worstState([down, ok, down])).toEqual({
      tone: 'bad',
      label: '2 down',
    })
  })
})

describe('projectNodes', () => {
  const service = (id: number, name: string) => ({ id, name, slug: name })

  test('lists every project with its services, then services in none', () => {
    const nodes = projectNodes(
      [
        {
          id: 1,
          name: 'Shop',
          slug: 'shop',
          projects: [service(4, 'web'), service(5, 'api')],
        },
        { id: 2, name: 'Empty', slug: 'empty', projects: [] },
      ],
      [service(4, 'web'), service(9, 'blog'), service(5, 'api')]
    )
    expect(nodes.map((node) => node.key)).toEqual([
      'project:1',
      'project:2',
      'service:9',
    ])
    expect(nodes[0].kind === 'project' && nodes[0].services).toEqual([
      service(4, 'web'),
      service(5, 'api'),
    ])
  })

  test('no project at all leaves the services alone', () => {
    expect(projectNodes([], [service(9, 'blog')])).toEqual([
      { kind: 'service', key: 'service:9', service: service(9, 'blog') },
    ])
  })
})

describe('capRows', () => {
  const rows = [1, 2, 3, 4, 5]

  test('a short list is shown whole', () => {
    expect(capRows(rows, 5, () => false)).toEqual({ shown: rows, more: 0 })
  })

  test('a long list is cut, and says how many are left out', () => {
    expect(capRows(rows, 3, () => false)).toEqual({
      shown: [1, 2, 3],
      more: 2,
    })
  })

  test('the row shown is kept when it falls past the limit', () => {
    expect(capRows(rows, 3, (row) => row === 5)).toEqual({
      shown: [1, 2, 5],
      more: 2,
    })
    expect(capRows(rows, 3, (row) => row === 2).shown).toEqual([1, 2, 3])
  })
})
