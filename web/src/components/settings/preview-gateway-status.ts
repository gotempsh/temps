// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { GatewayStatus } from '@/api/client'

export type GatewayStatusTone = 'ok' | 'warning' | 'error' | 'muted'

/**
 * What the gateway card's status row says: the saved on/off switch first,
 * then the container's own state. `enabled` is `undefined` until settings
 * load, which reads as enabled (the default).
 */
export function gatewayStatusSummary(
  enabled: boolean | undefined,
  status: Pick<GatewayStatus, 'running' | 'present'>
): { label: string; tone: GatewayStatusTone } {
  if (enabled === false) {
    // A removal that failed leaves the container in place; say so rather
    // than claim previews are off.
    return status.present
      ? {
          label: 'Disabled, but its container is still present',
          tone: 'warning',
        }
      : { label: 'Disabled', tone: 'muted' }
  }
  if (status.running) return { label: 'Running', tone: 'ok' }
  return status.present
    ? { label: 'Stopped', tone: 'warning' }
    : { label: 'Not deployed', tone: 'error' }
}
