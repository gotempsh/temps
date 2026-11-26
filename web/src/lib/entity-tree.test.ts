// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  capRows,
  databaseRequests,
  databaseState,
  domainState,
  locateInTree,
  pickProjects,
  projectNodes,
  countedState,
  serverState,
  serviceState,
  withRecent,
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

describe('countedState', () => {
  test('counts trouble, since one of several is not the whole server', () => {
    expect(
      countedState([serverState('active'), serverState('offline')])
    ).toEqual({ tone: 'bad', label: '1 offline' })
    expect(
      countedState([
        serverState('offline'),
        serverState('offline'),
        databaseState('starting'),
      ])
    ).toEqual({ tone: 'bad', label: '2 offline' })
    expect(
      countedState([databaseState('running'), databaseState('failed')])
    ).toEqual({ tone: 'bad', label: '1 failed' })
  })

  test('is healthy once anything is known and nothing is wrong', () => {
    expect(
      countedState([serverState('active'), databaseState('stopped')])
    ).toEqual({ tone: 'ok', label: 'Healthy' })
    expect(countedState([])).toBeUndefined()
    expect(countedState([undefined])).toBeUndefined()
  })
})

describe('databaseRequests', () => {
  const services = (from: number, count: number) =>
    Array.from({ length: count }, (_, index) => ({
      id: from + index,
      name: `s${from + index}`,
      slug: `s${from + index}`,
    }))

  test('asks for every service of the open projects, in order', () => {
    const { requests, truncated } = databaseRequests(
      [
        { key: 'a', services: services(1, 2) },
        { key: 'b', services: services(10, 1) },
      ],
      25
    )
    expect(
      requests.map((request) => [request.key, request.service.id])
    ).toEqual([
      ['a', 1],
      ['a', 2],
      ['b', 10],
    ])
    expect(truncated.size).toBe(0)
  })

  test('never asks for more than the limit, and says which projects it cut', () => {
    const { requests, truncated } = databaseRequests(
      [
        { key: 'a', services: services(1, 3) },
        { key: 'b', services: services(10, 3) },
        { key: 'c', services: services(20, 1) },
      ],
      4
    )
    expect(requests).toHaveLength(4)
    expect([...truncated]).toEqual(['b', 'c'])
    expect(databaseRequests([], 4).requests).toEqual([])
  })
})

describe('pickProjects', () => {
  const nodes = Array.from({ length: 12 }, (_, index) => ({
    key: `p${index}`,
  }))
  const keys = (picked: { shown: { key: string }[] }) =>
    picked.shown.map((node) => node.key)
  const none = { isCurrent: () => false, inTrouble: () => false, recent: [] }

  test('lists every project when they fit, as they are', () => {
    const few = nodes.slice(0, 8)
    expect(pickProjects(few, 8, none)).toEqual({ shown: few, more: 0 })
  })

  test('lists trouble first, then the recent, the rest in their order', () => {
    const picked = pickProjects(nodes, 5, {
      isCurrent: () => false,
      inTrouble: (node) => node.key === 'p9',
      recent: ['p11', 'p4'],
    })
    expect(keys(picked)).toEqual(['p9', 'p0', 'p1', 'p4', 'p11'])
    expect(picked.more).toBe(7)
  })

  test('the project shown is always listed, however full the list', () => {
    const picked = pickProjects(nodes, 3, {
      isCurrent: (node) => node.key === 'p10',
      inTrouble: (node) => ['p1', 'p2', 'p3'].includes(node.key),
      recent: [],
    })
    expect(keys(picked)).toContain('p10')
    expect(picked.shown).toHaveLength(3)
  })

  test('a recent project that no longer exists is ignored', () => {
    expect(keys(pickProjects(nodes, 2, { ...none, recent: ['gone'] }))).toEqual(
      ['p0', 'p1']
    )
  })
})

describe('withRecent', () => {
  test('puts the project first, once, and remembers at most eight', () => {
    expect(withRecent(['a', 'b', 'c'], 'b')).toEqual(['b', 'a', 'c'])
    const many = Array.from({ length: 8 }, (_, index) => `p${index}`)
    expect(withRecent(many, 'new')).toEqual(['new', ...many.slice(0, 7)])
  })
})
