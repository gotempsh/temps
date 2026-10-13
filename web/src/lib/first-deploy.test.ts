// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  SAMPLE_APP,
  firstDeployFailureHint,
  firstDeployPhase,
  firstDeployTrackingPath,
  hasCompletedDeployment,
  sampleProjectName,
  startSampleDeploy,
} from './first-deploy'

const environments = [
  { id: 21, name: 'Production', slug: 'production', is_preview: false },
]

function sampleDeps(
  options: {
    envs?: typeof environments
    createError?: Error
    deployError?: Error
  } = {}
) {
  const created: Array<{ name: string; imageRef: string; port: number }> = []
  const deploys: Array<{
    projectId: number
    environmentId: number
    imageRef: string
  }> = []
  return {
    created,
    deploys,
    deps: {
      createProject: async (body: (typeof created)[number]) => {
        if (options.createError) throw options.createError
        created.push(body)
        return { id: 7, slug: body.name }
      },
      listEnvironments: async () => options.envs ?? environments,
      deployImage: async (args: (typeof deploys)[number]) => {
        if (options.deployError) throw options.deployError
        deploys.push(args)
      },
    },
  }
}

describe('sampleProjectName', () => {
  test('uses the base name when it is free', () => {
    expect(sampleProjectName([])).toBe('hello-temps')
    expect(sampleProjectName(['api', 'web'])).toBe('hello-temps')
  })

  test('appends the first free numeric suffix', () => {
    expect(sampleProjectName(['hello-temps'])).toBe('hello-temps-2')
    expect(
      sampleProjectName(['Hello-Temps', 'hello-temps-2', 'hello-temps-4'])
    ).toBe('hello-temps-3')
  })
})

describe('hasCompletedDeployment', () => {
  test('requires the server to confirm a deployment reached ready', () => {
    expect(hasCompletedDeployment(undefined)).toBe(false)
    expect(hasCompletedDeployment({ has_completed_deployment: false })).toBe(
      false
    )
    expect(hasCompletedDeployment({})).toBe(false)
  })

  test('is true only after a deployment became ready', () => {
    expect(hasCompletedDeployment({ has_completed_deployment: true })).toBe(
      true
    )
  })
})

describe('firstDeployPhase', () => {
  test('maps terminal success states', () => {
    expect(firstDeployPhase('completed')).toBe('succeeded')
    expect(firstDeployPhase('superseded')).toBe('succeeded')
  })

  test('maps terminal failure states', () => {
    expect(firstDeployPhase('failed')).toBe('failed')
    expect(firstDeployPhase('cancelled')).toBe('failed')
    expect(firstDeployPhase('stopped')).toBe('failed')
  })

  test('treats everything else, including unknown states, as in progress', () => {
    for (const status of [
      'pending',
      'running',
      'deploying',
      'built',
      undefined,
    ]) {
      expect(firstDeployPhase(status)).toBe('in_progress')
    }
  })
})

describe('firstDeployFailureHint', () => {
  test('recognises an unreachable Docker daemon', () => {
    const hint = firstDeployFailureHint(
      'Failed to create container: error trying to connect: No such file or directory (os error 2) at /var/run/docker.sock'
    )
    expect(hint.kind).toBe('docker_unavailable')
    expect(hint.remedy).toContain('Start Docker')
    expect(
      firstDeployFailureHint(
        'Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?'
      ).kind
    ).toBe('docker_unavailable')
  })

  test('recognises image pull failures', () => {
    for (const reason of [
      'Failed to pull image nginx:alpine: pull access denied for nginx',
      'manifest for nginx:does-not-exist not found: manifest unknown',
      'toomanyrequests: You have reached your pull rate limit',
      'Get "https://registry-1.docker.io/v2/": dial tcp: lookup registry-1.docker.io: no such host',
    ]) {
      expect(firstDeployFailureHint(reason).kind).toBe('image_pull')
    }
  })

  test('falls back to reading the logs for anything else', () => {
    expect(firstDeployFailureHint('Health check failed after 300s').kind).toBe(
      'other'
    )
    expect(firstDeployFailureHint(null).kind).toBe('other')
    expect(firstDeployFailureHint(undefined).remedy).toContain('logs')
  })
})

describe('firstDeployTrackingPath', () => {
  test('encodes the project slug', () => {
    expect(firstDeployTrackingPath('hello-temps')).toBe(
      '/get-started?project=hello-temps'
    )
    expect(firstDeployTrackingPath('a b&c')).toBe(
      '/get-started?project=a+b%26c'
    )
  })
})

describe('startSampleDeploy', () => {
  test('creates the sample project and deploys its image to production', async () => {
    const r = sampleDeps()
    const result = await startSampleDeploy('hello-temps', r.deps)

    expect(result).toEqual({
      status: 'started',
      projectSlug: 'hello-temps',
      environmentName: 'Production',
    })
    expect(r.created).toEqual([
      {
        name: 'hello-temps',
        imageRef: SAMPLE_APP.image,
        port: SAMPLE_APP.port,
      },
    ])
    expect(r.deploys).toEqual([
      { projectId: 7, environmentId: 21, imageRef: SAMPLE_APP.image },
    ])
  })

  test('propagates project creation errors without deploying', async () => {
    const r = sampleDeps({ createError: new Error('name taken') })
    await expect(startSampleDeploy('hello-temps', r.deps)).rejects.toThrow(
      'name taken'
    )
    expect(r.deploys).toEqual([])
  })

  test('returns the project when the deployment fails to start', async () => {
    const error = new Error('Docker unavailable')
    const r = sampleDeps({ deployError: error })
    const result = await startSampleDeploy('hello-temps', r.deps)

    expect(result).toEqual({
      status: 'deploy_failed',
      projectSlug: 'hello-temps',
      error,
    })
  })

  test('reports a project without an environment', async () => {
    const r = sampleDeps({ envs: [] })
    const result = await startSampleDeploy('hello-temps', r.deps)

    expect(result).toEqual({
      status: 'no_environment',
      projectSlug: 'hello-temps',
    })
    expect(r.deploys).toEqual([])
  })
})
