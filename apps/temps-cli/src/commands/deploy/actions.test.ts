// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { parseDeploymentTarget } from './actions.js'

describe('parseDeploymentTarget', () => {
  test('parses numeric string IDs', () => {
    expect(parseDeploymentTarget({ projectId: '12', deploymentId: '345' })).toEqual({
      projectId: 12,
      deploymentId: 345,
    })
  })

  test('returns null when either ID is not numeric', () => {
    // A NaN in either slot would otherwise be interpolated straight into the
    // API path (e.g. /projects/NaN/deployments/345) instead of failing fast.
    expect(parseDeploymentTarget({ projectId: 'abc', deploymentId: '345' })).toBeNull()
    expect(parseDeploymentTarget({ projectId: '12', deploymentId: 'xyz' })).toBeNull()
    expect(parseDeploymentTarget({ projectId: '', deploymentId: '345' })).toBeNull()
  })
})
