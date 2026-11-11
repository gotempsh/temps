// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { DeploymentResponse, EnvironmentResponse } from '@/api/client'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import {
  canRedeployAnyInPlace,
  environmentsToRedeploy,
  redeployCurrentDeployments,
  redeploySummary,
  serviceLinkRedeployMessage,
  toastRedeployAfterLinkChange,
  type RedeployApi,
} from './service-link-redeploy'

installBrowserWindow()

const env = (
  id: number,
  name: string,
  current: number | null,
  isPreview = false
): EnvironmentResponse =>
  ({
    id,
    name,
    current_deployment_id: current,
    is_preview: isPreview,
  }) as EnvironmentResponse

const deployment = (
  id: number,
  environmentId: number,
  extra: Partial<DeploymentResponse>
): DeploymentResponse =>
  ({ id, environment_id: environmentId, ...extra }) as DeploymentResponse

describe('environmentsToRedeploy', () => {
  test('keeps non-preview environments with a running deployment', () => {
    const targets = environmentsToRedeploy([
      env(1, 'production', 10),
      env(2, 'staging', null),
      env(3, 'feature-x', 30, true),
    ])
    expect(targets.map((e) => e.name)).toEqual(['production'])
    expect(environmentsToRedeploy(undefined)).toEqual([])
  })
})

describe('serviceLinkRedeployMessage', () => {
  test('explains when linked variables reach the app', () => {
    expect(
      serviceLinkRedeployMessage({ kind: 'linked', serviceName: 'pg-main' }, [
        { name: 'production' },
      ])
    ).toEqual({
      title: 'pg-main was linked',
      description:
        'Its connection variables reach your app on its next deployment. Redeploy production to apply the change now.',
    })
  })

  test('explains that unlinking also needs a redeploy and names every environment', () => {
    const { title, description } = serviceLinkRedeployMessage(
      { kind: 'unlinked', serviceName: 'cache' },
      [{ name: 'production' }, { name: 'staging' }]
    )
    expect(title).toBe('cache was unlinked')
    expect(description).toContain('stay in the running app')
    expect(description).toContain(
      'Redeploy 2 environments (production, staging)'
    )
  })
})

describe('redeployCurrentDeployments', () => {
  function fakeApi(deployments: Record<number, DeploymentResponse>) {
    const calls: Array<{ fn: string; options: unknown }> = []
    const record =
      (fn: string) =>
      async (options: unknown): Promise<{ data: unknown }> => {
        calls.push({ fn, options })
        return { data: {} }
      }
    const api = {
      getDeployment: async (options: { path: { deployment_id: number } }) => ({
        data: deployments[options.path.deployment_id],
      }),
      triggerProjectPipeline: record('git'),
      deployFromImage: record('image'),
      deployFromStatic: record('static'),
    } as unknown as RedeployApi
    return { api, calls }
  }

  test('retries only failed environments after partial success', async () => {
    const { api, calls } = fakeApi({
      10: deployment(10, 1, { branch: 'main' }),
      20: deployment(20, 2, { branch: 'main' }),
    })
    const trigger = api.triggerProjectPipeline
    let fail = true
    api.triggerProjectPipeline = (async (
      options: Parameters<typeof trigger>[0]
    ) => {
      if (options?.body?.environment_id === 2 && fail)
        throw new Error('unavailable')
      return trigger(options)
    }) as typeof trigger
    const completed = new Set<number>()
    const environments = [env(1, 'production', 10), env(2, 'staging', 20)]
    const first = await redeployCurrentDeployments(
      1,
      'git',
      environments,
      api,
      completed
    )
    expect(first[1].failed).toBe('unavailable')
    expect(completed.has(1)).toBe(true)
    fail = false
    await redeployCurrentDeployments(1, 'git', environments, api, completed)
    expect(calls).toHaveLength(2)
    expect(completed.size).toBe(2)
  })

  test('rebuilds each environment from the source it is running', async () => {
    const { api, calls } = fakeApi({
      10: deployment(10, 1, { branch: 'main', commit_hash: 'abc123' }),
      20: deployment(20, 2, {
        metadata: {
          deploymentSourceType: 'docker_image',
          externalImageRef: 'registry.example/app:1',
          command: null,
          healthCheckPath: '/healthz',
        },
      } as Partial<DeploymentResponse>),
    })
    const outcomes = await redeployCurrentDeployments(
      7,
      'git',
      [env(1, 'production', 10), env(2, 'staging', 20)],
      api
    )
    expect(outcomes).toEqual([
      { environment: 'production', skipped: undefined },
      { environment: 'staging', skipped: undefined },
    ])
    expect(calls).toEqual([
      {
        fn: 'git',
        options: {
          path: { id: 7 },
          body: {
            branch: 'main',
            commit: 'abc123',
            tag: undefined,
            environment_id: 1,
          },
          throwOnError: true,
        },
      },
      {
        fn: 'image',
        options: {
          path: { project_id: 7, environment_id: 2 },
          body: {
            command: [],
            health_check_path: '/healthz',
            image_ref: 'registry.example/app:1',
          },
          throwOnError: true,
        },
      },
    ])
  })

  test('reports deployments that cannot be rebuilt instead of failing', async () => {
    const { api, calls } = fakeApi({
      10: deployment(10, 1, {
        metadata: { deploymentSourceType: 'uploaded_source' },
      } as Partial<DeploymentResponse>),
    })
    const outcomes = await redeployCurrentDeployments(
      7,
      'uploaded_source',
      [env(1, 'production', 10)],
      api
    )
    expect(calls).toEqual([])
    expect(outcomes[0].skipped).toContain('uploaded archive')
    expect(redeploySummary(outcomes)).toEqual({
      started: 0,
      message:
        'production not redeployed: it was deployed from an uploaded archive; upload it again',
    })
  })
})

describe('redeploySummary', () => {
  test('summarises started environments', () => {
    expect(
      redeploySummary([{ environment: 'production', skipped: undefined }])
    ).toEqual({ started: 1, message: 'Redeploying production' })
    expect(
      redeploySummary([
        { environment: 'production' },
        { environment: 'staging' },
      ]).message
    ).toBe('Redeploying 2 environments')
    expect(redeploySummary([]).message).toBe('Nothing to redeploy')
  })
})

describe('canRedeployAnyInPlace', () => {
  const apiFor = (
    deployments: Record<number, DeploymentResponse | 'fail'>
  ): Pick<RedeployApi, 'getDeployment'> =>
    ({
      getDeployment: async ({ path }: { path: { deployment_id: number } }) => {
        const found = deployments[path.deployment_id]
        if (found === 'fail' || found === undefined) throw new Error('boom')
        return { data: found }
      },
    }) as unknown as Pick<RedeployApi, 'getDeployment'>

  test('a manual project running an image can still be redeployed', async () => {
    const api = apiFor({
      31: deployment(31, 3, { metadata: { externalImageRef: 'app:1' } }),
    })
    expect(
      await canRedeployAnyInPlace(7, 'manual', [env(3, 'production', 31)], api)
    ).toBe(true)
  })

  test('a manual project running a Git ref can still be redeployed', async () => {
    const api = apiFor({
      31: deployment(31, 3, { branch: 'main', commit_hash: 'abc123' }),
    })
    expect(
      await canRedeployAnyInPlace(7, 'manual', [env(3, 'production', 31)], api)
    ).toBe(true)
  })

  test('an uploaded archive cannot be redeployed in place', async () => {
    const api = apiFor({ 31: deployment(31, 3, {}) })
    expect(
      await canRedeployAnyInPlace(
        7,
        'uploaded_source',
        [env(3, 'production', 31)],
        api
      )
    ).toBe(false)
  })

  test('a Git project whose live deployment is an uploaded archive cannot', async () => {
    const api = apiFor({
      31: deployment(31, 3, {
        metadata: { deploymentSourceType: 'uploaded_source' },
      }),
    })
    expect(
      await canRedeployAnyInPlace(7, 'git', [env(3, 'production', 31)], api)
    ).toBe(false)
  })

  test('one redeployable environment is enough to offer the action', async () => {
    const api = apiFor({
      31: deployment(31, 3, {}),
      41: deployment(41, 4, { metadata: { externalImageRef: 'app:2' } }),
    })
    expect(
      await canRedeployAnyInPlace(
        7,
        'uploaded_source',
        [env(3, 'production', 31), env(4, 'staging', 41)],
        api
      )
    ).toBe(true)
  })

  test('a deployment that cannot be read does not withhold the action', async () => {
    const api = apiFor({ 31: 'fail' })
    expect(
      await canRedeployAnyInPlace(
        7,
        'uploaded_source',
        [env(3, 'production', 31)],
        api
      )
    ).toBe(true)
  })

  test('a stalled lookup does not hold back the answer', async () => {
    const api = {
      getDeployment: () => new Promise(() => {}),
    } as unknown as Pick<RedeployApi, 'getDeployment'>
    const started = Date.now()
    expect(
      await canRedeployAnyInPlace(
        7,
        'uploaded_source',
        [env(3, 'production', 31)],
        api,
        30
      )
    ).toBe(true)
    expect(Date.now() - started).toBeLessThan(1000)
  })

  test('answers once one environment is redeployable, without waiting for the rest, and cancels them', async () => {
    let cancelled = false
    const api = {
      getDeployment: ({
        path,
        signal,
      }: {
        path: { deployment_id: number }
        signal?: AbortSignal
      }) => {
        if (path.deployment_id === 41) {
          return Promise.resolve({
            data: deployment(41, 4, {
              metadata: { externalImageRef: 'app:2' },
            }),
          })
        }
        return new Promise((_, reject) => {
          signal?.addEventListener('abort', () => {
            cancelled = true
            reject(new Error('aborted'))
          })
        })
      },
    } as unknown as Pick<RedeployApi, 'getDeployment'>
    expect(
      await canRedeployAnyInPlace(
        7,
        'uploaded_source',
        [env(3, 'production', 31), env(4, 'staging', 41)],
        api,
        5000
      )
    ).toBe(true)
    expect(cancelled).toBe(true)
  })

  test('nothing to decide is not a reason to withhold it', async () => {
    expect(
      await canRedeployAnyInPlace(7, 'uploaded_source', [], apiFor({}))
    ).toBe(true)
  })
})

describe('serviceLinkRedeployMessage for a source that cannot be redeployed', () => {
  test('says to upload the source again instead of offering a redeploy', () => {
    const { description } = serviceLinkRedeployMessage(
      { kind: 'linked', serviceName: 'synthetic-db' },
      [{ name: 'production' }],
      false
    )
    expect(description).toContain('Upload the source again')
    expect(description).not.toContain('Redeploy production')
  })
})

describe('toastRedeployAfterLinkChange', () => {
  const change = { kind: 'linked', serviceName: 'synthetic-db' } as const
  const notifier = () => {
    const info: { title: string; options: Record<string, unknown> }[] = []
    return {
      info,
      notify: {
        info: (title: string, options: Record<string, unknown>) =>
          info.push({ title, options }),
        success: () => {},
        warning: () => {},
        error: () => {},
      },
    }
  }

  test('an uploaded source gets a notice without a Redeploy action', async () => {
    const api = mockApi({
      'GET /projects/7/environments': () =>
        jsonResponse([env(3, 'production', 31)]),
      'GET /projects/7/deployments/31': () =>
        jsonResponse(deployment(31, 3, {})),
    })
    const { info, notify } = notifier()
    await toastRedeployAfterLinkChange(
      { id: 7, name: 'synthetic-api', source_type: 'uploaded_source' },
      change,
      notify as never
    )
    expect(info).toHaveLength(1)
    expect(info[0].options.action).toBeUndefined()
    expect(info[0].options.description).toContain('Upload the source again')
    api.restore()
  })

  test('a Git project still gets the Redeploy now action', async () => {
    const api = mockApi({
      'GET /projects/7/environments': () =>
        jsonResponse([env(3, 'production', 31)]),
      'GET /projects/7/deployments/31': () =>
        jsonResponse(
          deployment(31, 3, { branch: 'main', commit_hash: 'abc123' })
        ),
    })
    const { info, notify } = notifier()
    await toastRedeployAfterLinkChange(
      { id: 7, name: 'synthetic-api', source_type: 'git' },
      change,
      notify as never
    )
    expect(info[0].options.action).toMatchObject({ label: 'Redeploy now' })
    // Eligibility came from reading the live deployment, not from a failed read.
    expect(api.called('GET', '/projects/7/deployments/31')).toBe(true)
    api.restore()
  })

  test('a manual project running an image still gets the Redeploy now action', async () => {
    const api = mockApi({
      'GET /projects/7/environments': () =>
        jsonResponse([env(3, 'production', 31)]),
      'GET /projects/7/deployments/31': () =>
        jsonResponse(
          deployment(31, 3, { metadata: { externalImageRef: 'app:1' } })
        ),
    })
    const { info, notify } = notifier()
    await toastRedeployAfterLinkChange(
      { id: 7, name: 'synthetic-api', source_type: 'manual' },
      change,
      notify as never
    )
    expect(info[0].options.action).toMatchObject({ label: 'Redeploy now' })
    api.restore()
  })
})
