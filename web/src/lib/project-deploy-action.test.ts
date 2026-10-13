// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  AUTO_REFRESH_MAX_POLLS,
  defaultDeployEnvironment,
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
