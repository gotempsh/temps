// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { PluginTier, ServicePluginResponse } from '@/api/client/types.gen'
import type { StatusTone } from '@temps-sdk/ds'

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
