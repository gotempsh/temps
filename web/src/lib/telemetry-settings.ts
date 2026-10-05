// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  TelemetryEventCategory,
  TelemetryEventInfo,
  TelemetryStatusResponse,
} from '@/api/client/types.gen'
import type { StatusTone } from '@temps-sdk/ds'
import type { QueryClient } from '@tanstack/react-query'
import { getTelemetrySettingsQueryKey } from '@/api/client/@tanstack/react-query.gen'

/** Apply an acknowledged save after cancelling older status snapshots. */
export async function cacheSavedTelemetryPreference(
  queryClient: QueryClient,
  status: TelemetryStatusResponse
): Promise<void> {
  const queryKey = getTelemetrySettingsQueryKey()
  await queryClient.cancelQueries({ queryKey })
  queryClient.setQueryData(queryKey, status)
}

/** Display order and wording for event categories. */
export const TELEMETRY_CATEGORY_LABELS: Record<TelemetryEventCategory, string> =
  {
    instance: 'Instance lifecycle',
    deployments: 'Deployments',
    projects: 'Projects & environments',
    git: 'Git providers',
    domains: 'Domains & certificates',
    services: 'Managed services & backups',
    feature_activation: 'First use of a feature',
    ai: 'AI features',
    configuration: 'Configuration',
    health: 'Instance health',
  }

const CATEGORY_ORDER = Object.keys(
  TELEMETRY_CATEGORY_LABELS
) as TelemetryEventCategory[]

export interface TelemetryEventGroup {
  category: TelemetryEventCategory
  label: string
  events: string[]
}

/** Group the server's event catalog by category, in a stable order. */
export function groupTelemetryEvents(
  events: TelemetryEventInfo[]
): TelemetryEventGroup[] {
  const byCategory = new Map<TelemetryEventCategory, string[]>()
  for (const event of events) {
    const names = byCategory.get(event.category) ?? []
    names.push(event.name)
    byCategory.set(event.category, names)
  }
  return CATEGORY_ORDER.filter((category) => byCategory.has(category)).map(
    (category) => ({
      category,
      label: TELEMETRY_CATEGORY_LABELS[category],
      events: byCategory.get(category) ?? [],
    })
  )
}

export interface TelemetryStateSummary {
  tone: StatusTone
  label: string
  /** One sentence saying what decided the current state. */
  reason: string
}

/** Headline state and the reason for it, per decision source. */
export function summarizeTelemetryState(
  status: Pick<
    TelemetryStatusResponse,
    'enabled' | 'source' | 'env_var' | 'default_enabled'
  >
): TelemetryStateSummary {
  const tone: StatusTone = status.enabled ? 'ok' : 'idle'
  const label = status.enabled ? 'Sending' : 'Off'
  switch (status.source) {
    case 'environment':
      return {
        tone,
        label,
        reason: `Forced off by the ${status.env_var} environment variable on this server. It overrides the setting below.`,
      }
    case 'admin_setting':
      return {
        tone,
        label,
        reason: status.enabled
          ? 'An admin turned telemetry on for this instance.'
          : 'An admin turned telemetry off for this instance.',
      }
    case 'default':
      return {
        tone,
        label,
        reason: status.default_enabled
          ? 'On by default. Nobody has changed this setting yet.'
          : 'Off by default. Nobody has changed this setting yet.',
      }
    case 'unavailable':
      return {
        tone: 'warn',
        label: 'Unavailable',
        reason:
          'The telemetry reporter could not start on this server, so nothing is sent. Check the server log for the cause.',
      }
  }
}

export interface TelemetryToggleState {
  checked: boolean
  disabled: boolean
  /** Why the switch is disabled; `null` when it can be used. */
  disabledReason: string | null
}

/**
 * The switch reflects what is actually happening, and is only usable when a
 * change would take effect and the caller may make it.
 */
export function telemetryToggleState(
  status: Pick<
    TelemetryStatusResponse,
    | 'enabled'
    | 'source'
    | 'env_var'
    | 'can_manage'
    | 'admin_preference'
    | 'default_enabled'
  >,
  isSaving: boolean
): TelemetryToggleState {
  if (!status.can_manage) {
    return {
      checked: status.admin_preference ?? status.default_enabled,
      disabled: true,
      disabledReason: 'Only instance admins can change this setting.',
    }
  }
  return {
    checked: status.admin_preference ?? status.default_enabled,
    disabled: isSaving,
    disabledReason: null,
  }
}
