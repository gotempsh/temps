// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { startFirstImageDeploy } from './first-image-deploy'

const environments = [
  { id: 11, name: 'Staging', slug: 'staging', is_preview: false },
  { id: 12, name: 'Production', slug: 'production', is_preview: false },
]

function recorder(envs = environments) {
  const deploys: Array<{
    projectId: number
    environmentId: number
    imageRef: string
  }> = []
  let listed = 0
  return {
    deploys,
    listedCount: () => listed,
    deps: {
      listEnvironments: async () => {
        listed++
        return envs
      },
      deployImage: async (args: (typeof deploys)[number]) => {
        deploys.push(args)
      },
    },
  }
}

describe('startFirstImageDeploy', () => {
  test('deploys the entered image to production', async () => {
    const r = recorder()
    const result = await startFirstImageDeploy(5, '  nginx:1.27  ', r.deps)

    expect(result).toEqual({ status: 'started', environmentName: 'Production' })
    expect(r.deploys).toEqual([
      { projectId: 5, environmentId: 12, imageRef: 'nginx:1.27' },
    ])
  })

  test('does nothing when no image was entered', async () => {
    for (const image of [undefined, '', '   ']) {
      const r = recorder()
      expect(await startFirstImageDeploy(5, image, r.deps)).toEqual({
        status: 'skipped',
      })
      expect(r.listedCount()).toBe(0)
      expect(r.deploys).toHaveLength(0)
    }
  })

  test('reports a project with no environment instead of deploying', async () => {
    const r = recorder([])
    expect(await startFirstImageDeploy(5, 'nginx', r.deps)).toEqual({
      status: 'no_environment',
    })
    expect(r.deploys).toHaveLength(0)
  })

  test('propagates deploy failures to the caller', async () => {
    const failing = {
      listEnvironments: async () => environments,
      deployImage: async () => {
        throw { detail: 'image not found' }
      },
    }
    await expect(startFirstImageDeploy(5, 'nginx', failing)).rejects.toEqual({
      detail: 'image not found',
    })
  })
})
