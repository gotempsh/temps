// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { DeploymentResponse } from '@/api/client'
import { recentDeploymentsRefetchInterval } from './recent-deployments'

function deployment(status: string): DeploymentResponse {
  return { status } as DeploymentResponse
}

describe('recent deployments refresh behavior', () => {
  test('polls while any deployment is still changing', () => {
    expect(
      recentDeploymentsRefetchInterval([
        deployment('completed'),
        deployment('building'),
      ])
    ).toBe(2500)
  })

  test('stops polling once every deployment is terminal', () => {
    expect(
      recentDeploymentsRefetchInterval([
        deployment('completed'),
        deployment('failed'),
      ])
    ).toBe(false)
    expect(recentDeploymentsRefetchInterval(undefined)).toBe(false)
  })
})
