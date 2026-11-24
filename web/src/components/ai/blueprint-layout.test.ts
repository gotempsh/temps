// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  NODE_WIDTH,
  blueprintChanges,
  deployOrder,
  layoutBlueprint,
} from './blueprint-layout'

const parts = [
  { id: 'worker', kind: 'app' as const, role: 'worker' },
  { id: 'browser', kind: 'client' as const },
  { id: 'db', kind: 'data' as const },
  { id: 'api', kind: 'app' as const, role: 'api' },
  { id: 'web', kind: 'app' as const, role: 'web' },
  { id: 'mail', kind: 'external' as const },
]
const flows = [
  { from: 'browser', to: 'web', kind: 'http', label: 'pages' },
  { from: 'web', to: 'api', kind: 'http', label: '/api' },
  { from: 'api', to: 'db', kind: 'query', label: 'tasks' },
  { from: 'worker', to: 'mail', kind: 'http', label: 'send' },
  { from: 'web', to: 'nowhere', kind: 'http', label: 'dangling' },
]

describe('layoutBlueprint', () => {
  test('puts each kind in its own column, in the order data travels', () => {
    const layout = layoutBlueprint(parts, flows)
    const x = (id: string) => layout.nodes.find((n) => n.id === id)?.x ?? -1
    expect(x('browser')).toBeLessThan(x('web'))
    expect(x('web')).toBe(x('api'))
    expect(x('api')).toBeLessThan(x('db'))
    expect(x('db')).toBeLessThan(x('mail'))
    expect(layout.columns.map((c) => c.kind)).toEqual([
      'client',
      'app',
      'data',
      'external',
    ])
  })

  test('orders apps by role: pages, then the API, then workers', () => {
    const layout = layoutBlueprint(parts, flows)
    const y = (id: string) => layout.nodes.find((n) => n.id === id)?.y ?? -1
    expect(y('web')).toBeLessThan(y('api'))
    expect(y('api')).toBeLessThan(y('worker'))
  })

  test('draws only flows between parts that exist', () => {
    const layout = layoutBlueprint(parts, flows)
    expect(layout.edges.map((e) => e.index)).toEqual([0, 1, 2, 3])
    expect(layout.edges[0].path.startsWith('M ')).toBe(true)
  })

  test('skips empty columns so the diagram stays narrow', () => {
    const layout = layoutBlueprint(
      [
        { id: 'web', kind: 'app', role: 'static' },
        { id: 'browser', kind: 'client' },
      ],
      []
    )
    expect(layout.columns).toHaveLength(2)
    expect(layout.width).toBeLessThan(3 * NODE_WIDTH + 200)
  })

  test('an empty design has no size', () => {
    expect(layoutBlueprint([], [])).toMatchObject({ width: 0, height: 0 })
  })
})

describe('layoutBlueprint flows', () => {
  // Two apps that both use the same three stores: every flow from one app
  // crosses a flow from the other, which is where labels used to collide.
  const shared = [
    { id: 'ui', kind: 'app' as const, role: 'web' },
    { id: 'parser', kind: 'app' as const, role: 'worker' },
    { id: 'db', kind: 'data' as const },
    { id: 'queue', kind: 'data' as const },
    { id: 'store', kind: 'data' as const },
  ]
  const sharedFlows = [
    { from: 'ui', to: 'db' },
    { from: 'ui', to: 'queue' },
    { from: 'ui', to: 'store' },
    { from: 'parser', to: 'db' },
    { from: 'parser', to: 'queue' },
    { from: 'parser', to: 'store' },
  ]
  const start = (path: string) => Number(path.split(' ')[2])
  const end = (path: string) => {
    const tokens = path.split(' ')
    return Number(tokens[tokens.length - 1])
  }

  test('flows leave a part from separate points, top flow highest', () => {
    const layout = layoutBlueprint(shared, sharedFlows)
    const fromUi = layout.edges.filter((e) => e.from === 'ui')
    const starts = fromUi.map((e) => start(e.path))
    expect(new Set(starts).size).toBe(3)
    // Ordered like the parts they go to, so the fan never crosses itself.
    expect([...starts].sort((a, b) => a - b)).toEqual(starts)
  })

  test('flows arriving at one part get their own points too', () => {
    const layout = layoutBlueprint(shared, sharedFlows)
    const intoDb = layout.edges.filter((e) => e.to === 'db')
    expect(new Set(intoDb.map((e) => end(e.path))).size).toBe(2)
  })

  test('every label sits by its own arrowhead, never on another', () => {
    const layout = layoutBlueprint(shared, sharedFlows)
    const spots = layout.edges.map((e) => `${e.labelX},${e.labelY}`)
    expect(new Set(spots).size).toBe(layout.edges.length)
    const node = (id: string) => layout.nodes.find((n) => n.id === id)
    for (const edge of layout.edges) {
      const target = node(edge.to)
      expect(edge.labelAnchor).toBe('end')
      expect(edge.labelX).toBeLessThan(target?.x ?? 0)
      expect(edge.labelY).toBeLessThan(end(edge.path))
    }
  })

  test('a store sits level with the app that uses it', () => {
    const layout = layoutBlueprint(
      [
        { id: 'web', kind: 'app', role: 'web' },
        { id: 'worker', kind: 'app', role: 'worker' },
        // Listed in the opposite order to the apps that use them.
        { id: 'jobs', kind: 'data' },
        { id: 'pages', kind: 'data' },
      ],
      [
        { from: 'web', to: 'pages' },
        { from: 'worker', to: 'jobs' },
      ]
    )
    const y = (id: string) => layout.nodes.find((n) => n.id === id)?.y ?? -1
    expect(y('pages')).toBeLessThan(y('jobs'))
  })

  test('a flow back to an earlier column is labelled on its arrow side', () => {
    const layout = layoutBlueprint(
      [
        { id: 'web', kind: 'app', role: 'web' },
        { id: 'db', kind: 'data' },
      ],
      [{ from: 'db', to: 'web' }]
    )
    const [edge] = layout.edges
    const web = layout.nodes.find((n) => n.id === 'web')
    expect(edge.labelAnchor).toBe('start')
    expect(edge.labelX).toBeGreaterThan((web?.x ?? 0) + (web?.width ?? 0))
  })
})

describe('blueprintChanges', () => {
  test('names what a proposal adds, changes and removes', () => {
    const previous = {
      components: [
        { id: 'web', name: 'Web' },
        { id: 'db', name: 'DB' },
        { id: 'old', name: 'Old' },
      ],
      flows: [{ from: 'web', to: 'db', kind: 'query', label: 'tasks' }],
    }
    const next = {
      components: [
        { id: 'web', name: 'Web app' },
        { id: 'db', name: 'DB' },
        { id: 'cache', name: 'Cache' },
      ],
      flows: [
        { from: 'web', to: 'db', kind: 'query', label: 'tasks' },
        { from: 'web', to: 'cache', kind: 'cache', label: 'sessions' },
      ],
    }
    const changes = blueprintChanges(previous, next)
    expect([...changes.added]).toEqual(['cache'])
    expect([...changes.changed]).toEqual(['web'])
    expect(changes.removed.map((c) => c.id)).toEqual(['old'])
    expect([...changes.addedFlows]).toEqual([1])
    expect(changes.removedFlows).toEqual([])
  })

  test('a first design has nothing to compare against', () => {
    const changes = blueprintChanges(null, { components: [], flows: [] })
    expect(changes.added.size).toBe(0)
  })
})

describe('deployOrder', () => {
  test('deploys an API before the front end that calls it', () => {
    expect(deployOrder(parts, flows)).toEqual(['worker', 'api', 'web'])
  })

  test('a cycle still lists every app once', () => {
    const order = deployOrder(
      [
        { id: 'a', kind: 'app' },
        { id: 'b', kind: 'app' },
      ],
      [
        { from: 'a', to: 'b' },
        { from: 'b', to: 'a' },
      ]
    )
    expect(order.sort()).toEqual(['a', 'b'])
  })
})
