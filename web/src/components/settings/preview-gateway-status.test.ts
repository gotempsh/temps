// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import type {
  GatewayStatus,
  PreviewGatewaySettingsResponse,
} from '@/api/client'

import {
  gatewayStatusSummary,
  reloadGatewayStateAfterFailure,
} from './preview-gateway-status'

describe('reloadGatewayStateAfterFailure', () => {
  const enabledSettings: PreviewGatewaySettingsResponse = {
    enabled: true,
    image: '',
    host_port: 8090,
    auto_upgrade: true,
    container_name: 'temps-preview-gateway',
    default_image: 'ghcr.io/example/preview-gateway@sha256:0000',
    default_host_port: 8090,
    default_container_name: 'temps-preview-gateway',
  }
  const missingGateway: GatewayStatus = {
    auto_upgrade: true,
    container_name: 'temps-preview-gateway',
    drift: false,
    expected_image: 'ghcr.io/example/preview-gateway@sha256:0000',
    health: 'missing',
    present: false,
    running: false,
  }

  test('returns what the server saved although the action failed', async () => {
    // An enable whose gateway failed to start: the switch is saved, so the
    // card must stop treating the gateway as disabled and offer Restart.
    const reloaded = await reloadGatewayStateAfterFailure({
      status: async () => ({ data: missingGateway }),
      settings: async () => ({ data: enabledSettings }),
    })

    expect(reloaded.settings?.enabled).toBe(true)
    expect(reloaded.status).toBe(missingGateway)
  })

  test('leaves out what cannot be read instead of throwing', async () => {
    const reloaded = await reloadGatewayStateAfterFailure({
      status: () => {
        throw new Error('network unreachable')
      },
      settings: async () => ({ data: undefined }),
    })

    expect(reloaded).toEqual({ status: undefined, settings: undefined })
  })
})

describe('gatewayStatusSummary', () => {
  test('a disabled gateway reads as disabled, not as missing', () => {
    expect(
      gatewayStatusSummary(false, { running: false, present: false })
    ).toEqual({ label: 'Disabled', tone: 'muted' })
  })

  test('a disabled gateway whose container survived says so', () => {
    expect(
      gatewayStatusSummary(false, { running: true, present: true })
    ).toEqual({
      label: 'Disabled, but its container is still present',
      tone: 'warning',
    })
  })

  test('an enabled gateway reports its container state', () => {
    expect(
      gatewayStatusSummary(true, { running: true, present: true })
    ).toEqual({ label: 'Running', tone: 'ok' })
    expect(
      gatewayStatusSummary(true, { running: false, present: true })
    ).toEqual({ label: 'Stopped', tone: 'warning' })
    expect(
      gatewayStatusSummary(undefined, { running: false, present: false })
    ).toEqual({ label: 'Not deployed', tone: 'error' })
  })
})
