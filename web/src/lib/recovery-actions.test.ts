// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { DeploymentResponse } from '@/api/client'
import {
  recoveryActionCopy,
  runRecoveryAction,
  type RecoveryApi,
} from './recovery-actions'

type Call = { fn: string; options: unknown }

function fakeApi(deployment: Partial<DeploymentResponse> = {}) {
  const calls: Call[] = []
  const record =
    (fn: string, data: unknown = {}) =>
    async (options: unknown) => {
      calls.push({ fn, options })
      return { data }
    }
  const api = {
    getDeployment: record('getDeployment', {
      id: 30,
      environment_id: 4,
      branch: 'main',
      commit_hash: 'abc',
      metadata: { deploymentSourceType: 'git' },
      ...deployment,
    }),
    triggerProjectPipeline: record('triggerProjectPipeline'),
    deployFromImage: record('deployFromImage'),
    deployFromStatic: record('deployFromStatic'),
    rollbackToDeployment: record('rollbackToDeployment'),
    restartContainer: record('restartContainer'),
  } as unknown as RecoveryApi
  return { api, calls }
}

describe('runRecoveryAction', () => {
  test('redeploys a git deployment from its own commit', async () => {
    const { api, calls } = fakeApi()
    await runRecoveryAction(
      { kind: 'redeploy', projectId: 1, sourceType: 'git', deploymentId: 30 },
      api
    )
    expect(calls.map((call) => call.fn)).toEqual([
      'getDeployment',
      'triggerProjectPipeline',
    ])
    expect(calls[1].options).toMatchObject({
      path: { id: 1 },
      body: { branch: 'main', commit: 'abc', environment_id: 4 },
    })
  })

  test('rejects with the reason when the source cannot be redeployed', async () => {
    const { api } = fakeApi({
      metadata: { deploymentSourceType: 'uploaded_source' },
    } as Partial<DeploymentResponse>)
    await expect(
      runRecoveryAction(
        {
          kind: 'redeploy',
          projectId: 1,
          sourceType: 'manual',
          deploymentId: 30,
        },
        api
      )
    ).rejects.toThrow("Deployment #30 can't be redeployed")
  })

  test('rolls back to the target deployment', async () => {
    const { api, calls } = fakeApi()
    await runRecoveryAction(
      {
        kind: 'rollback',
        projectId: 1,
        targetDeploymentId: 15,
        targetIsLive: false,
      },
      api
    )
    expect(calls).toEqual([
      {
        fn: 'rollbackToDeployment',
        options: {
          path: { project_id: 1, deployment_id: 15 },
          throwOnError: true,
        },
      },
    ])
  })

  test('restarts the container by its Docker ID', async () => {
    const { api, calls } = fakeApi()
    await runRecoveryAction(
      {
        kind: 'restart_container',
        projectId: 1,
        environmentId: 4,
        containerId: 'f00dcafe',
        containerName: 'web',
      },
      api
    )
    expect(calls[0]).toEqual({
      fn: 'restartContainer',
      options: {
        path: { project_id: 1, environment_id: 4, container_id: 'f00dcafe' },
        throwOnError: true,
      },
    })
  })
})

describe('recoveryActionCopy', () => {
  test('explains rolling back to a deployment that is still live', () => {
    const copy = recoveryActionCopy({
      kind: 'rollback',
      projectId: 1,
      targetDeploymentId: 15,
      targetIsLive: true,
    })
    expect(copy.title).toBe('Roll back to #15?')
    expect(copy.description).toContain('#15 is still serving traffic')
  })

  test('names the container being restarted', () => {
    const copy = recoveryActionCopy({
      kind: 'restart_container',
      projectId: 1,
      environmentId: 4,
      containerId: 'f00dcafe',
      containerName: 'web',
    })
    expect(copy.title).toBe('Restart web?')
    expect(copy.errorTitle).toBe('Failed to restart web')
  })
})
