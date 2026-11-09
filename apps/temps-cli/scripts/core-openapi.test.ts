// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { coreOpenApi, isPluginPath } from './core-openapi'

const ref = (name: string) => ({ $ref: `#/components/schemas/${name}` })

test('removes plugin operations and exclusive transitive schemas while retaining shared core definitions', () => {
  const spec = {
    openapi: '3.1.0',
    paths: {
      '/projects': ref('Core'),
      '/x/plugins': ref('Plugin'),
      '/x/plugins/install/progress/{id}': ref('Progress'),
      '/xray': ref('Xray'),
    },
    components: {
      schemas: {
        Core: ref('Shared'),
        Shared: { type: 'string' },
        Plugin: {
          allOf: [ref('Shared'), ref('PluginChild'), ref('OrphanDependency')],
        },
        PluginChild: ref('Plugin'),
        Progress: { type: 'object' },
        Xray: { type: 'string' },
        Orphan: ref('OrphanDependency'),
        OrphanDependency: { type: 'number' },
      },
    },
  }
  const original = structuredClone(spec)
  const result = coreOpenApi(spec) as typeof spec
  expect(Object.keys(result.paths)).toEqual(['/projects', '/xray'])
  expect(Object.keys(result.components.schemas).sort()).toEqual([
    'Core',
    'Orphan',
    'OrphanDependency',
    'Shared',
    'Xray',
  ])
  expect(spec).toEqual(original)
  expect(coreOpenApi(result)).toEqual(result)
})

test('recognizes only the external plugin namespace', () => {
  expect(['/x', '/x/plugins', '/x/plugins/a/grants'].every(isPluginPath)).toBe(
    true
  )
  expect(['/xray', '/api/x', '/projects'].some(isPluginPath)).toBe(false)
})

test('retains core AI consent operations and their schemas without importing plugin APIs', () => {
  const spec = {
    paths: {
      '/x/plugins/{name}/ai-tools': { get: ref('Consent'), put: ref('Update') },
      '/x/reports/tools/list': { get: ref('Report') },
    },
    components: {
      schemas: {
        Consent: ref('Tool'),
        Update: { type: 'object' },
        Tool: { type: 'object' },
        Report: { type: 'object' },
      },
    },
  }
  const result = coreOpenApi(spec) as typeof spec
  expect(Object.keys(result.paths)).toEqual(['/x/plugins/{name}/ai-tools'])
  expect(result.paths['/x/plugins/{name}/ai-tools']).toEqual(
    spec.paths['/x/plugins/{name}/ai-tools']
  )
  expect(Object.keys(result.components.schemas).sort()).toEqual([
    'Consent',
    'Tool',
    'Update',
  ])
  expect(coreOpenApi(result)).toEqual(result)
})

test('preserves documents without components and lets the caller reject malformed paths', () => {
  expect(coreOpenApi({ paths: { '/core': {}, '/x': {} } })).toEqual({
    paths: { '/core': {} },
  })
  for (const spec of [null, {}, { paths: null }])
    expect(coreOpenApi(spec)).toEqual(spec)
})
