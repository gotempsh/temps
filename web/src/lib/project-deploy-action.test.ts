import { describe, expect, test } from 'bun:test'
import {
  deploymentsAfterStartPath,
  projectDeployLaunchMode,
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
