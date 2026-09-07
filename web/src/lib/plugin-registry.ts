// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

export function pluginInstallAction(
  installedVersion: string | undefined,
  registryVersion: string
): PluginInstallAction {
  if (installedVersion === undefined) return 'install'
  return installedVersion === registryVersion ? 'installed' : 'upgrade'
}
