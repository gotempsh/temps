// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  GatewayStatus,
  PreviewGatewaySettingsResponse,
} from '@/api/client'

export type GatewayStatusTone = 'ok' | 'warning' | 'error' | 'muted'

/** The gateway as the server reports it; a part that could not be read is left out. */
export interface GatewayServerState {
  status?: GatewayStatus
  settings?: PreviewGatewaySettingsResponse
}

/**
 * Read the gateway's status and saved settings again after an action failed.
 * A failed action can still have changed both: a save stores the settings
 * before it switches the gateway on or off, and a restart removes the old
 * container before it creates the new one. The saved `enabled` decides which
 * actions the card offers, so a stale copy can block the very action the
 * error asks for. Never throws, so the failed action's error stays the one
 * shown.
 */
export async function reloadGatewayStateAfterFailure(load: {
  status: () => Promise<{ data?: GatewayStatus }>
  settings: () => Promise<{ data?: PreviewGatewaySettingsResponse }>
}): Promise<GatewayServerState> {
  const [status, settings] = await Promise.allSettled([
    Promise.resolve().then(load.status),
    Promise.resolve().then(load.settings),
  ])
  return {
    status: status.status === 'fulfilled' ? status.value.data : undefined,
    settings: settings.status === 'fulfilled' ? settings.value.data : undefined,
  }
}

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

/**
 * Why the gateway is not running, when the server's last attempt to set it
 * up failed: the error Docker gave, and when Temps tries again on its own.
 * `null` when nothing failed, so the card says nothing extra.
 */
export function gatewaySetupProblem(
  status: Pick<GatewayStatus, 'setup_error' | 'next_setup_attempt_at'>,
  formatTime: (at: Date) => string = (at) => at.toLocaleTimeString()
): { error: string; retry: string } | null {
  if (!status.setup_error) return null
  const next = status.next_setup_attempt_at
    ? new Date(status.next_setup_attempt_at)
    : null
  const retry =
    next && !Number.isNaN(next.getTime())
      ? `Temps tries again on its own at ${formatTime(next)}; Restart tries now.`
      : 'Temps tries again on its own within a minute; Restart tries now.'
  return { error: status.setup_error, retry }
}
