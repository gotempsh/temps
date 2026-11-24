// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { BlueprintDocument } from '@/api/client/types.gen'
import {
  cleanDocument,
  formatHolds,
  newComponent,
  parseHolds,
  removeComponent,
  savedProblems,
  slugify,
  uniqueId,
  updateComponent,
  withKind,
} from './blueprint-editor'

const doc: BlueprintDocument = {
  summary: 'Tasks for small teams',
  components: [
    {
      id: 'web',
      name: 'Web',
      purpose: 'Pages',
      kind: 'app',
      role: 'web',
      runtime: 'preset',
      stack: 'Vite',
    },
    {
      id: 'db',
      name: 'DB',
      purpose: 'Tasks',
      kind: 'data',
      engine: 'postgres',
      holds: ['tasks'],
    },
  ],
  flows: [{ from: 'web', to: 'db', kind: 'query', label: 'tasks' }],
}

describe('slugify', () => {
  test('makes a lowercase slug that starts with a letter', () => {
    expect(slugify('Orders Worker')).toBe('orders-worker')
    expect(slugify('  API (v2) ')).toBe('api-v2')
    expect(slugify('3D viewer')).toBe('part-3d-viewer')
  })
})

describe('uniqueId', () => {
  test('numbers an id that is taken', () => {
    expect(uniqueId('Web', ['web', 'web-2'])).toBe('web-3')
    expect(uniqueId('Cache', ['web'])).toBe('cache')
  })
})

describe('withKind', () => {
  test('keeps only the fields of the new kind', () => {
    const asData = withKind(doc.components[0], 'data')
    expect(asData).toEqual({
      id: 'web',
      name: 'Web',
      purpose: 'Pages',
      kind: 'data',
      engine: 'postgres',
      holds: [],
    })
    expect('stack' in asData).toBe(false)
  })

  test('a new app gets a role and a runtime', () => {
    const app = newComponent('app', 'Orders API', ['web'])
    expect(app).toMatchObject({
      id: 'orders-api',
      role: 'web',
      runtime: 'preset',
    })
  })
})

describe('editing parts', () => {
  test('renaming an id moves its flows with it', () => {
    const next = updateComponent(doc, 'db', {
      ...doc.components[1],
      id: 'tasks-db',
    })
    expect(next.flows?.[0]).toMatchObject({ from: 'web', to: 'tasks-db' })
  })

  test('removing a part removes its flows', () => {
    const next = removeComponent(doc, 'db')
    expect(next.components.map((c) => c.id)).toEqual(['web'])
    expect(next.flows).toEqual([])
  })

  test('cleaning drops an image the app does not run and blank holds', () => {
    const draft: BlueprintDocument = {
      ...doc,
      components: [
        { ...doc.components[0], image: 'nats:2.10', name: ' Web ' },
        { ...doc.components[1], holds: ['tasks', ' ', 'users '] },
      ],
    }
    const clean = cleanDocument(draft)
    expect('image' in clean.components[0]).toBe(false)
    expect(clean.components[0].name).toBe('Web')
    expect(clean.components[1].holds).toEqual(['tasks', 'users'])
  })
})

describe('holds text', () => {
  test('round-tripping a typed list is stable', () => {
    const typed = 'tasks, users'
    expect(parseHolds(typed)).toEqual(['tasks', 'users'])
    expect(formatHolds(parseHolds(typed))).toBe(typed)
    expect(formatHolds(parseHolds(formatHolds(parseHolds(typed))))).toBe(typed)
  })

  test('spacing and a comma still being typed do not leak into the list', () => {
    expect(parseHolds('tasks,users')).toEqual(['tasks', 'users'])
    expect(parseHolds('  tasks ,   users  ')).toEqual(['tasks', 'users'])
    expect(parseHolds('tasks, ')).toEqual(['tasks'])
    expect(parseHolds('tasks,,users')).toEqual(['tasks', 'users'])
    expect(parseHolds('')).toEqual([])
  })

  test('an unset list starts the field empty', () => {
    expect(formatHolds(undefined)).toBe('')
    expect(formatHolds(null)).toBe('')
    expect(formatHolds(['tasks'])).toBe('tasks')
  })
})

describe('savedProblems', () => {
  test('reads the problems a rejected save lists', () => {
    expect(savedProblems({ problems: ['a', 2, 'b'] })).toEqual(['a', 'b'])
    expect(savedProblems(new Error('x'))).toEqual([])
    expect(savedProblems(null)).toEqual([])
  })
})
