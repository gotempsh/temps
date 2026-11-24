// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  isSandboxDeployTool,
  parseSandboxDeployStarted,
  sandboxDeploySummary,
} from './sandbox-deploy'

const started = JSON.stringify({
  status: 'started',
  deployment_id: 41,
  project_id: 39,
  environment_id: 39,
  state: 'pending',
  files: 48,
  left_out: 471,
  instruction: 'Temps is building the project from those files.',
})

describe('isSandboxDeployTool', () => {
  test('matches the plain and MCP-qualified names', () => {
    expect(isSandboxDeployTool('temps_deploy_sandbox_source')).toBe(true)
    expect(
      isSandboxDeployTool('mcp__temps-chat__temps_deploy_sandbox_source')
    ).toBe(true)
    expect(isSandboxDeployTool('sandbox_exec')).toBe(false)
  })
})

describe('parseSandboxDeployStarted', () => {
  test('reads the deployment the tool started', () => {
    expect(parseSandboxDeployStarted(started)).toEqual({
      deploymentId: 41,
      projectId: 39,
      environmentId: 39,
      files: 48,
      leftOut: 471,
    })
  })

  test('is null while running and for a refusal or failure', () => {
    expect(parseSandboxDeployStarted(undefined)).toBeNull()
    expect(parseSandboxDeployStarted(null)).toBeNull()
    expect(
      parseSandboxDeployStarted(
        'Nothing was deployed: project 39 does not accept uploaded source.'
      )
    ).toBeNull()
    expect(
      parseSandboxDeployStarted(
        JSON.stringify({ status: 'started', project_id: 39 })
      )
    ).toBeNull()
  })
})

describe('sandboxDeploySummary', () => {
  test('says what was sent and what was left out', () => {
    const parsed = parseSandboxDeployStarted(started)
    expect(parsed && sandboxDeploySummary(parsed)).toBe(
      'Deploying 48 files from the sandbox (471 left out: dependencies, build output, credentials)'
    )
  })

  test('omits the left-out note when nothing was left out', () => {
    expect(
      sandboxDeploySummary({
        deploymentId: 1,
        projectId: 2,
        environmentId: null,
        files: 1,
        leftOut: 0,
      })
    ).toBe('Deploying 1 file from the sandbox')
  })
})
