// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { cliSpec } from './openapi-canonical'
test('CLI projection removes browser progress schemas while retaining shared errors', () => {
  const ref = (name: string) => ({ $ref: '#/components/schemas/' + name })
  const source = {
    paths: {
      '/x/plugins/install/progress/{id}': {
        response: ref('Progress'),
        error: ref('Problem'),
      },
      '/ordinary': { response: ref('Problem') },
    },
    components: {
      schemas: {
        Progress: { step: ref('Step') },
        Step: { type: 'string' },
        Problem: { type: 'object' },
      },
    },
  }
  const projected = cliSpec(source) as {
    paths: Record<string, unknown>
    components: { schemas: Record<string, unknown> }
  }
  expect(projected.paths).toEqual({ '/ordinary': source.paths['/ordinary'] })
  expect(projected.components.schemas).toEqual({ Problem: { type: 'object' } })
  expect(source.components.schemas.Progress).toBeDefined()
})
