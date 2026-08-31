// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  composeRevisionForRedeploy,
  deploymentsAfterStartPath,
  projectDeployLaunchMode,
} from './project-deploy-action'

describe('project header deploy action', () => {
  test('opens a dialog in place for deployable project sources', () => {
    expect(projectDeployLaunchMode('git')).toBe('dialog')
    expect(projectDeployLaunchMode('docker_image')).toBe('dialog')
    expect(projectDeployLaunchMode('compose')).toBe('dialog')
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

  test('redeploys a compose deployment from its immutable saved revision', () => {
    expect(
      composeRevisionForRedeploy({ metadata: { sourceBundleId: 42 } })
    ).toBe(42)
    expect(composeRevisionForRedeploy({ metadata: null })).toBeUndefined()
  })
})
