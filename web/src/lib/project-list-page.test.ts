// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  MAX_SERVICES_PER_REQUEST,
  projectListPage,
  serviceWindow,
} from './project-list-page'
import { deployedAt, orderProjectList } from './project-order'

type Svc = { id: number; last_deployment: string | null }
type Grp = { id: number; projects: Svc[] }

const build = {
  group: (group: Grp) => `g${group.id}`,
  service: (service: Svc) => `s${service.id}`,
}

/** What the server does: newest first, never deployed last, stable. */
function serverOrder(services: Svc[]): Svc[] {
  return services
    .map((service, index) => ({ service, index }))
    .sort((a, b) => {
      const at = deployedAt(a.service.last_deployment)
      const bt = deployedAt(b.service.last_deployment)
      return at === bt ? a.index - b.index : bt - at
    })
    .map((entry) => entry.service)
}

/** One page as the Projects page reads it: the window, then the cut. */
function pageOf(groups: Grp[], ordered: Svc[], page: number, size: number) {
  const window = serviceWindow(page, size, groups.length)
  const fetched = window.pages.flatMap((p) =>
    ordered.slice((p - 1) * window.perPage, p * window.perPage)
  )
  return projectListPage(
    groups,
    fetched,
    { offset: window.offset, page, pageSize: size },
    build
  )
}

const day = (n: number) => `2026-11-${String(n).padStart(2, '0')}T00:00:00Z`
const svc = (id: number, at: string | null): Svc => ({
  id,
  last_deployment: at,
})

describe('serviceWindow', () => {
  test('with no projects, a page is one request of its own size', () => {
    expect(serviceWindow(1, 9, 0)).toEqual({
      perPage: 9,
      pages: [1],
      offset: 0,
    })
    expect(serviceWindow(3, 9, 0)).toEqual({
      perPage: 9,
      pages: [3],
      offset: 18,
    })
  })

  test('reaches back as many services as there are projects', () => {
    // Page 2 of 9 with 2 projects: services 7 to 17 can land on it.
    const window = serviceWindow(2, 9, 2)
    expect(window.offset).toBeLessThanOrEqual(7)
    expect(
      window.offset + window.pages.length * window.perPage
    ).toBeGreaterThanOrEqual(18)
    expect(window.pages.length).toBeLessThanOrEqual(2)
  })

  test('never asks for more than the API serves in one page', () => {
    const window = serviceWindow(4, 72, 250)
    expect(window.perPage).toBe(MAX_SERVICES_PER_REQUEST)
    // 250 projects reach back past the first service: read from it.
    expect(window.offset).toBe(0)
    expect(
      window.offset + window.pages.length * window.perPage
    ).toBeGreaterThanOrEqual(4 * 72)
  })
})

describe('projectListPage', () => {
  test('a page holds exactly its size, projects and services together', () => {
    // The reported case: 2 projects and 8 services, 9 a page.
    const groups: Grp[] = [
      { id: 1, projects: [svc(100, day(26))] },
      { id: 2, projects: [svc(200, null)] },
    ]
    const services = serverOrder([
      svc(1, day(25)),
      svc(2, day(23)),
      svc(3, day(21)),
      svc(4, day(19)),
      svc(5, day(17)),
      svc(6, day(7)),
      svc(7, null),
      svc(8, null),
    ])
    expect(pageOf(groups, services, 1, 9)).toEqual([
      'g1',
      's1',
      's2',
      's3',
      's4',
      's5',
      's6',
      'g2',
      's7',
    ])
    expect(pageOf(groups, services, 2, 9)).toEqual(['s8'])
  })

  test('a project falls on the page its deployment puts it on', () => {
    const groups: Grp[] = [{ id: 1, projects: [svc(100, day(5))] }]
    const services = serverOrder(
      Array.from({ length: 12 }, (_, i) => svc(i + 1, day(28 - i)))
    )
    // Services deployed on the 28th down to the 17th: the project (5th) is
    // last, on page 2.
    expect(pageOf(groups, services, 1, 9)).toEqual([
      's1',
      's2',
      's3',
      's4',
      's5',
      's6',
      's7',
      's8',
      's9',
    ])
    expect(pageOf(groups, services, 2, 9)).toEqual(['s10', 's11', 's12', 'g1'])
  })

  test('more projects than a page holds spill onto the next', () => {
    const groups: Grp[] = Array.from({ length: 5 }, (_, i) => ({
      id: i + 1,
      projects: [svc(100 + i, day(28))],
    }))
    const services = serverOrder([svc(1, day(1))])
    expect(pageOf(groups, services, 1, 3)).toEqual(['g1', 'g2', 'g3'])
    expect(pageOf(groups, services, 2, 3)).toEqual(['g4', 'g5', 's1'])
  })

  test('every page matches the whole list cut into pages', () => {
    // Few distinct times, so ties and never-deployed items are common.
    let seed = 7
    const random = (n: number) => {
      seed = (seed * 1103515245 + 12345) % 2147483648
      return seed % n
    }
    const times = [null, day(1), day(2), day(3), day(4)]
    for (let round = 0; round < 300; round++) {
      let id = 0
      const groups: Grp[] = Array.from({ length: random(14) }, () => ({
        id: ++id,
        projects: Array.from({ length: random(3) }, () =>
          svc(1000 + ++id, times[random(times.length)])
        ),
      }))
      const services = serverOrder(
        Array.from({ length: random(40) }, () =>
          svc(++id, times[random(times.length)])
        )
      )
      const whole = orderProjectList(groups, services, build)
      for (const size of [3, 9, 18]) {
        const pages = Math.max(1, Math.ceil(whole.length / size))
        for (let page = 1; page <= pages; page++)
          expect(pageOf(groups, services, page, size)).toEqual(
            whole.slice((page - 1) * size, page * size)
          )
      }
    }
  })
})
