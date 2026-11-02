// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  initialUploadEnvironment,
  pendingUploadEnvironments,
  projectUploadPath,
  uploadDeployPath,
  AUTO_REFRESH_MAX_POLLS,
  defaultDeployEnvironment,
  initialDeployBranch,
  deploymentsAfterStartPath,
  imageDeployRetryPath,
  projectDeployLaunchMode,
  projectDeploysImage,
  shouldStopAutoRefresh,
} from './project-deploy-action'

describe('project header deploy action', () => {
  test('opens a dialog in place for deployable project sources', () => {
    expect(projectDeployLaunchMode('git')).toBe('dialog')
    expect(projectDeployLaunchMode('docker_image')).toBe('dialog')
  })

  test('keeps file-backed projects in their upload flow', () => {
    expect(projectDeployLaunchMode('uploaded_source')).toBe('upload')
    expect(projectDeployLaunchMode('static_files')).toBe('upload')
  })

  test('only targets deployments after a deployment starts', () => {
    expect(deploymentsAfterStartPath('my-project')).toBe(
      '/projects/my-project/deployments?autoRefresh=true'
    )
  })
})

describe('projectDeploysImage', () => {
  test('docker image projects always deploy an image', () => {
    expect(projectDeploysImage({ source_type: 'docker_image' })).toBe(true)
    expect(
      projectDeploysImage({ source_type: 'docker_image', repo_name: 'app' })
    ).toBe(true)
  })

  test('flexible projects without a repository deploy an image', () => {
    expect(projectDeploysImage({ source_type: 'manual' })).toBe(true)
    expect(
      projectDeploysImage({ source_type: 'manual', repo_name: null })
    ).toBe(true)
    expect(
      projectDeploysImage({ source_type: 'manual', repo_name: '  ' })
    ).toBe(true)
  })

  test('flexible projects with a repository use the git pipeline', () => {
    expect(
      projectDeploysImage({ source_type: 'manual', repo_name: 'app' })
    ).toBe(false)
  })

  test('other sources never take the image path', () => {
    expect(projectDeploysImage({ source_type: 'git' })).toBe(false)
    expect(projectDeploysImage({ source_type: 'static_files' })).toBe(false)
    expect(projectDeploysImage({ source_type: 'uploaded_source' })).toBe(false)
  })
})

describe('defaultDeployEnvironment', () => {
  const env = (id: number, slug: string, is_preview = false) => ({
    id,
    slug,
    is_preview,
  })

  test('returns undefined when there are no environments', () => {
    expect(defaultDeployEnvironment(undefined)).toBeUndefined()
    expect(defaultDeployEnvironment([])).toBeUndefined()
  })

  test('prefers production over other environments', () => {
    expect(
      defaultDeployEnvironment([env(1, 'staging'), env(2, 'production')])?.id
    ).toBe(2)
  })

  test('falls back to the first non-preview environment', () => {
    expect(
      defaultDeployEnvironment([env(1, 'pr-12', true), env(2, 'staging')])?.id
    ).toBe(2)
  })

  test('falls back to a preview environment when it is the only one', () => {
    expect(defaultDeployEnvironment([env(7, 'pr-12', true)])?.id).toBe(7)
  })
})

describe('imageDeployRetryPath', () => {
  test('opens the image dialog with the reference encoded', () => {
    const path = imageDeployRetryPath('my-app', 'ghcr.io/org/app:v1 2')
    const url = new URL(path, 'http://console.local')
    expect(url.pathname).toBe('/projects/my-app/deployments')
    expect(url.searchParams.get('deploy')).toBe('true')
    expect(url.searchParams.get('image')).toBe('ghcr.io/org/app:v1 2')
  })
})

describe('shouldStopAutoRefresh', () => {
  test('stops as soon as a new deployment appears', () => {
    expect(
      shouldStopAutoRefresh({ initialCount: 1, currentCount: 2, polls: 1 })
    ).toBe(true)
  })

  test('keeps polling while waiting, within the bound', () => {
    expect(
      shouldStopAutoRefresh({ initialCount: 1, currentCount: 1, polls: 3 })
    ).toBe(false)
  })

  test('stops after the bound when the deployment was already listed', () => {
    expect(
      shouldStopAutoRefresh({
        initialCount: 1,
        currentCount: 1,
        polls: AUTO_REFRESH_MAX_POLLS,
      })
    ).toBe(true)
  })
})

describe('initialDeployBranch', () => {
  test('starts on the branch the chosen environment deploys from', () => {
    expect(initialDeployBranch({ branch: 'release' }, 'main', 'main')).toBe(
      'release'
    )
  })

  test('falls back to the branch passed in, then the main branch', () => {
    expect(initialDeployBranch({ branch: null }, 'develop', 'main')).toBe(
      'develop'
    )
    expect(initialDeployBranch({ branch: '  ' }, undefined, 'main')).toBe(
      'main'
    )
    expect(initialDeployBranch(undefined, undefined, undefined)).toBe('')
  })
})

describe('uploading for a chosen environment', () => {
  const production = { id: 1, slug: 'production', is_preview: false }
  const staging = { id: 2, slug: 'staging', is_preview: false }

  test('the upload link names the environment when one was asked for', () => {
    expect(projectUploadPath('example', 2)).toBe(
      '/projects/example/drop?environment=2'
    )
    expect(projectUploadPath('example')).toBe('/projects/example/drop')
  })

  test('the upload page starts on it rather than on production', () => {
    expect(initialUploadEnvironment([production, staging], '2')).toBe(staging)
  })

  test('a missing or stale environment falls back to the usual default', () => {
    expect(initialUploadEnvironment([production, staging], null)).toBe(
      production
    )
    expect(initialUploadEnvironment([production, staging], '99')).toBe(
      production
    )
  })
})

describe('a run of uploads', () => {
  const environments = [
    { id: 1, name: 'production' },
    { id: 2, name: 'staging' },
    { id: 3, name: 'qa' },
  ]

  test('the address carries every environment still waiting', () => {
    expect(projectUploadPath('example', 2, [2, 3])).toBe(
      '/projects/example/drop?environment=2&pending=2&pending=3'
    )
  })

  test('a deploy request from an upload project keeps what is waiting', () => {
    expect(
      uploadDeployPath(
        { slug: 'example', source_type: 'uploaded_source' },
        { environmentId: 2, pendingEnvironmentIds: [2, 3] }
      )
    ).toBe('/projects/example/drop?environment=2&pending=2&pending=3')
    expect(
      uploadDeployPath(
        { slug: 'example', source_type: 'git' },
        { environmentId: 2, pendingEnvironmentIds: [2, 3] }
      )
    ).toBeNull()
  })

  test('what is left keeps its order, without the one just handled', () => {
    expect(
      pendingUploadEnvironments(environments, ['3', '1', '2'], 1).map(
        ({ name }) => name
      )
    ).toEqual(['qa', 'staging'])
  })

  test('an environment that no longer exists is not waited for', () => {
    expect(pendingUploadEnvironments(environments, ['9', 'x'])).toEqual([])
  })
})
