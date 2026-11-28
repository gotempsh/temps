// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  AccessDifference,
  PluginTier,
  ServicePluginResponse,
} from '@/api/client/types.gen'
import type { StatusTone } from '@temps-sdk/ds'
import { pluginPermissionLabels } from './plugin-grants'

/** What the console says about a plugin that runs as a Temps service. */
export function servicePluginSummary(plugin: ServicePluginResponse): {
  tone: StatusTone
  label: string
  detail: string
} {
  switch (plugin.state) {
    case 'running':
      return {
        tone: 'ok',
        label: 'Running',
        detail: "Temps is connected to the service's running deployment.",
      }
    case 'asleep':
      return {
        tone: 'idle',
        label: 'Stopped while idle',
        detail:
          plugin.detail ??
          'The service stops when idle and starts again on the next request to the plugin.',
      }
    case 'update_waiting':
      return {
        tone: 'warn',
        label: 'Update waiting',
        detail:
          plugin.detail ??
          'The service runs a build that was not approved. The plugin stays disconnected until the update is approved.',
      }
    case 'unavailable':
      return {
        tone: 'error',
        label: 'Unavailable',
        detail:
          plugin.detail ??
          'Temps is not connected to the service this plugin runs as.',
      }
  }
}

/** The page of the service a plugin runs as, while that service exists. */
export function servicePluginPath(
  plugin: ServicePluginResponse
): string | undefined {
  return plugin.service_slug ? `/projects/${plugin.service_slug}` : undefined
}

/** Where a plugin's service links the databases the plugin keeps its data in. */
export function servicePluginStoragePath(
  plugin: ServicePluginResponse
): string | undefined {
  return plugin.service_slug
    ? `/projects/${plugin.service_slug}/storage`
    : undefined
}

/** What a plugin's tier is called and what it means for what it can reach. */
export function pluginTierSummary(tier: PluginTier): {
  label: string
  detail: string
} {
  switch (tier) {
    case 'verified':
      return {
        label: 'Verified',
        detail:
          'From the signed registry and reviewed. It may be given more than a community plugin.',
      }
    case 'community':
      return {
        label: 'Community',
        detail:
          "Runs in its own container. It is never given Temps's database or data folder, and reaches Temps only with the permissions you grant.",
      }
  }
}

/** The service plugins an administrator has to act on, with the reason. */
export function servicePluginsNeedingAttention(
  plugins: ServicePluginResponse[]
): ServicePluginResponse[] {
  return plugins.filter((plugin) => plugin.state === 'unavailable')
}

/** The service plugins whose new build waits for an administrator. */
export function servicePluginsWithUpdate(
  plugins: ServicePluginResponse[]
): ServicePluginResponse[] {
  return plugins.filter((plugin) => plugin.pending_update != null)
}

/** One kind of access an update asks for, with what exactly. */
export type PluginUpdateRequest = { title: string; items: string[] }

/**
 * What an update asks for beyond the approved build, in the words the
 * permission screen uses. Empty when it asks for nothing more.
 */
export function pluginUpdateRequests(
  difference: AccessDifference
): PluginUpdateRequest[] {
  const capabilityLabels: Record<string, string> = {
    api_read: 'Read through the Temps API',
    api_write: 'Change things through the Temps API',
  }
  const groups: PluginUpdateRequest[] = [
    {
      title: 'New permissions it asks for',
      items: difference.added_permissions.map(
        (permission) => pluginPermissionLabels[permission].label
      ),
    },
    {
      title: 'New kinds of API calls it intends to make',
      items: difference.added_capabilities.map(
        (capability) => capabilityLabels[capability] ?? capability
      ),
    },
    {
      title: 'New routes open without signing in',
      items: difference.added_public_paths,
    },
    {
      title: 'New events it listens for',
      items: difference.added_events,
    },
  ]
  return groups.filter((group) => group.items.length > 0)
}

/** "1.0.0 → 1.1.0", or just the new version when the old one is unknown. */
export function pluginUpdateVersions(difference: AccessDifference): string {
  if (!difference.version_from) return `v${difference.version_to}`
  if (difference.version_from === difference.version_to)
    return `v${difference.version_to} (same version, new build)`
  return `v${difference.version_from} → v${difference.version_to}`
}

/** The server's reason for refusing a service, from its problem details. */
export function addServiceFailure(error: unknown): string {
  if (error && typeof error === 'object') {
    const problem = error as { detail?: unknown; title?: unknown }
    if (typeof problem.detail === 'string' && problem.detail !== '')
      return problem.detail
    if (typeof problem.title === 'string' && problem.title !== '')
      return problem.title
  }
  return 'The service could not be added as a plugin.'
}
