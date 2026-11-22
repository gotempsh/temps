// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { DeploymentResponse } from '@/api/client'
import {
  deploymentPollingInterval,
  deploymentReference,
  deploymentStatusPresentation,
  deploymentUrlNote,
  isActiveDeploymentStatus,
  matchingDeployment,
} from './deployment-card-state'

function deployment(
  id: number,
  overrides: Partial<DeploymentResponse> = {}
): DeploymentResponse {
  return {
    id,
    project_id: 7,
    environment_id: 11,
    environment: {
      id: 11,
      name: 'Production',
      slug: 'production',
      domains: [],
    },
    created_at: 1_788_000_000 + id,
    status: 'pending',
    is_current: false,
    url: '',
    commit_hash: 'abcdef1234567890',
    branch: 'main',
    ...overrides,
  }
}

describe('deployment action presentation', () => {
  test('prefers the confirmed server result over proposal parameters', () => {
    expect(
      deploymentReference(
        JSON.stringify({ id: 3, environment_id: 4, branch: 'stale' }),
        JSON.stringify({
          project_id: 7,
          environment_id: 11,
          branch: 'main',
          commit: 'abcdef1234567890',
        }),
        '2026-08-30T00:00:00Z'
      )
    ).toEqual({
      projectId: 7,
      deploymentId: null,
      environmentId: 11,
      branch: 'main',
      tag: null,
      commit: 'abcdef1234567890',
      createdAfterSeconds: 1_788_047_990,
    })
  })

  test('matches the newest deployment from the requested environment and commit', () => {
    const reference = deploymentReference(
      null,
      JSON.stringify({
        project_id: 7,
        environment_id: 11,
        commit: 'abcdef1',
      }),
      null
    )
    expect(reference).not.toBeNull()

    const found = matchingDeployment(
      [
        deployment(1),
        deployment(4, { environment_id: 12 }),
        deployment(3, { commit_hash: '0000000000000000' }),
        deployment(2),
      ],
      reference!
    )
    expect(found?.id).toBe(2)
  })

  test('uses the deployment id returned by a workspace Drop operation', () => {
    expect(
      deploymentReference(
        JSON.stringify({ project_id: 7 }),
        JSON.stringify({ id: 42, project_id: 7, environment_id: 11 }),
        null
      )
    ).toMatchObject({
      projectId: 7,
      deploymentId: 42,
      environmentId: 11,
    })
  })

  test('stops polling for terminal deployments and stale missing records', () => {
    expect(
      deploymentPollingInterval(
        'executed',
        'running',
        '2026-09-01T12:00:00Z',
        Date.parse('2026-09-01T12:00:30Z')
      )
    ).toBe(1500)
    expect(deploymentPollingInterval('executed', 'completed', null)).toBe(false)
    expect(
      deploymentPollingInterval(
        'executed',
        null,
        '2026-09-01T12:00:00Z',
        Date.parse('2026-09-01T12:03:00Z')
      )
    ).toBe(false)
    expect(deploymentPollingInterval('proposed', null, null)).toBe(false)
  })
})

describe('deployment status presentation', () => {
  const label = (status: string) => deploymentStatusPresentation(status).label

  test('a deployment a newer one replaced or stopped is finished, not queued', () => {
    expect(label('superseded')).toBe('Replaced')
    expect(label('stopped')).toBe('Stopped')
    expect(isActiveDeploymentStatus('superseded')).toBe(false)
    expect(isActiveDeploymentStatus('stopped')).toBe(false)
  })

  test('only a deployment that has not started reads Queued', () => {
    expect(label('pending')).toBe('Queued')
    for (const status of [
      'creating',
      'running',
      'in_progress',
      'built',
      'ready',
      'deploying',
    ]) {
      expect(label(status)).toBe('Deploying')
      expect(isActiveDeploymentStatus(status)).toBe(true)
    }
  })

  test('every successful state reads Ready', () => {
    for (const status of ['completed', 'deployed', 'success']) {
      expect(label(status)).toBe('Ready')
    }
  })

  test('an unknown state is shown as the server named it', () => {
    expect(label('rolling_back')).toBe('Rolling back')
    expect(label('')).toBe('Unknown')
  })

  test('the URL of a deployment that is not serving is not promised as coming', () => {
    expect(deploymentUrlNote('pending')).toBe('once live')
    expect(deploymentUrlNote('running')).toBe('once live')
    expect(deploymentUrlNote('stopped')).toBe('not serving')
    expect(deploymentUrlNote('superseded')).toBe('not serving')
    expect(deploymentUrlNote('failed')).toBe('not serving')
    expect(deploymentUrlNote('completed')).toBeNull()
  })
})
