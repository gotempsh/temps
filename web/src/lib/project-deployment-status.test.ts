// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { projectDeploymentStatus } from './project-deployment-status'

const live = [{ current_deployment_id: 7 }]
const nothingLive = [{ current_deployment_id: null }]

describe('projectDeploymentStatus', () => {
  test('is unknown until environments load', () => {
    expect(projectDeploymentStatus(undefined, { status: 'failed' })).toBe(
      undefined
    )
  })

  test('reports a live project as deployed', () => {
    expect(projectDeploymentStatus(live, { status: 'completed' })).toBe(
      'Deployed'
    )
  })

  test('keeps a live project deployed while a new build runs', () => {
    expect(projectDeploymentStatus(live, { status: 'running' })).toBe(
      'Deployed'
    )
  })

  test('reports a first build as deploying', () => {
    expect(projectDeploymentStatus(nothingLive, { status: 'pending' })).toBe(
      'Deploying'
    )
  })

  test('reports a failed latest deployment even when an older one is live', () => {
    expect(projectDeploymentStatus(live, { status: 'failed' })).toBe('Failed')
  })

  test('reports a failed first deployment as failed, not "not deployed"', () => {
    expect(projectDeploymentStatus(nothingLive, { status: 'failed' })).toBe(
      'Failed'
    )
  })

  test('reports unhealthy live containers as degraded', () => {
    expect(projectDeploymentStatus(live, { status: 'completed' }, 2)).toBe(
      'Degraded'
    )
  })

  test('prefers failed over degraded', () => {
    expect(projectDeploymentStatus(live, { status: 'failed' }, 1)).toBe(
      'Failed'
    )
  })

  test('shows degraded containers while a new build runs', () => {
    expect(projectDeploymentStatus(live, { status: 'running' }, 1)).toBe(
      'Degraded'
    )
  })

  test('ignores container problems when nothing is live', () => {
    expect(projectDeploymentStatus(nothingLive, null, 3)).toBe('Not deployed')
  })
})
