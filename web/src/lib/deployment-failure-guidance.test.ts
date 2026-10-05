// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  failureSettingsLink,
  failureStageLabel,
  failureTimeoutSummary,
} from './deployment-failure-guidance'

describe('deployment failure guidance', () => {
  test('labels every stage', () => {
    expect(failureStageLabel('health_check')).toBe('Health check')
    expect(failureStageLabel('dependency_install')).toBe('Dependency install')
  })

  test('deep-links project sections under the project', () => {
    expect(failureSettingsLink('deploy', 'my-app')).toEqual({
      href: '/projects/my-app/settings/build?tab=deploy',
      label: 'Deployment settings',
    })
    expect(failureSettingsLink('environment_variables', 'my-app')?.href).toBe(
      '/projects/my-app/settings/environment-variables'
    )
  })

  test('deep-links instance-wide sections outside the project', () => {
    expect(failureSettingsLink('docker_registry', 'my-app')?.href).toBe(
      '/settings/docker-registry'
    )
    expect(failureSettingsLink('build_limits', 'my-app')?.href).toBe(
      '/settings/build-limits'
    )
  })

  test('returns no link when the failure has no settings fix', () => {
    expect(failureSettingsLink(null, 'my-app')).toBeNull()
    expect(failureSettingsLink(undefined, 'my-app')).toBeNull()
  })

  test('summarises timeout limit and runtime', () => {
    expect(
      failureTimeoutSummary({
        timeout_limit_seconds: 300,
        timeout_elapsed_seconds: 303,
      })
    ).toBe('Limit 5 min · ran 5 min 3s')
    expect(
      failureTimeoutSummary({
        timeout_limit_seconds: 90,
        timeout_elapsed_seconds: null,
      })
    ).toBe('Limit 90s')
    expect(
      failureTimeoutSummary({
        timeout_limit_seconds: null,
        timeout_elapsed_seconds: null,
      })
    ).toBeNull()
  })
})
