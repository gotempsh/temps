// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReloadFailureResponse } from '@/api/client/types.gen'

/** Reload returns the same failure list for HTTP 207 and thrown HTTP 502 bodies. */
export function pluginReloadFailures(value: unknown): ReloadFailureResponse[] {
  if (!value || typeof value !== 'object' || !('failures' in value)) return []
  if (!Array.isArray(value.failures)) return []
  return value.failures.filter(
    (failure): failure is ReloadFailureResponse =>
      failure !== null &&
      typeof failure === 'object' &&
      (failure.plugin == null || typeof failure.plugin === 'string') &&
      typeof failure.reason === 'string'
  )
}

/**
 * Registry metadata is signed but remains untrusted display input. Only
 * absolute HTTP(S) URLs may become browser navigation or image targets.
 */
export function safeRegistryNavigationUrl(
  value?: string | null
): string | undefined {
  if (!value) return undefined

  try {
    const url = new URL(value)
    if (url.username || url.password) return undefined
    return url.protocol === 'https:' || url.protocol === 'http:'
      ? url.toString()
      : undefined
  } catch {
    return undefined
  }
}

export type PluginInstallAction = 'install' | 'upgrade' | 'installed'

const PLUGIN_ADMIN_ROLES = new Set(['admin', 'platform_admin'])

/** Mirrors the roles that carry `Permission::SystemAdmin` in temps-auth. */
export function canManageExternalPlugins(role?: string | null): boolean {
  return role !== undefined && role !== null && PLUGIN_ADMIN_ROLES.has(role)
}

export function pluginInstallAction(
  installedVersion: string | undefined,
  registryVersion: string
): PluginInstallAction {
  if (installedVersion === undefined) return 'install'
  return installedVersion === registryVersion ? 'installed' : 'upgrade'
}
