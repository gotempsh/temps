// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { coreOpenApi } from './core-openapi'
const ref = (name: string) => ({ $ref: `#/components/schemas/${name}` })
test('refresh imports core changes but not plugin routes or transitive plugin schemas', () => {
  const before = {
    paths: { '/x/old': ref('OldPlugin') },
    components: { schemas: { OldPlugin: { type: 'string' } } },
  }
  const fetched = {
    paths: {
      '/projects/security': ref('Policy'),
      '/x/new': ref('PluginProgress'),
      '/x/old': ref('PluginProgress'),
    },
    components: {
      schemas: {
        Policy: ref('Check'),
        Check: { enum: ['extends'] },
        PluginProgress: ref('PluginCatalog'),
        PluginCatalog: { type: 'object' },
      },
    },
  }
  const result = coreOpenApi(fetched, before)
  expect(result.paths).toEqual({
    '/projects/security': ref('Policy'),
    '/x/old': ref('OldPlugin'),
  })
  expect(result.components?.schemas).toEqual({
    OldPlugin: { type: 'string' },
    Policy: ref('Check'),
    Check: { enum: ['extends'] },
  })
})

test('refresh removes deleted and renamed schemas while retaining transitive dependencies', () => {
  const before = {
    paths: { '/project': ref('OldProject'), '/x/old': ref('LegacyPlugin') },
    components: {
      schemas: {
        OldProject: { type: 'string' },
        Deleted: { type: 'object' },
        LegacyPlugin: ref('LegacyDependency'),
        LegacyDependency: { type: 'string' },
      },
    },
  }
  const fetched = {
    paths: { '/project': ref('Project') },
    components: {
      schemas: {
        Project: ref('ProjectId'),
        ProjectId: { type: 'integer' },
      },
    },
  }
  expect(coreOpenApi(fetched, before).components?.schemas).toEqual({
    Project: ref('ProjectId'),
    ProjectId: { type: 'integer' },
    LegacyPlugin: ref('LegacyDependency'),
    LegacyDependency: { type: 'string' },
  })
})
