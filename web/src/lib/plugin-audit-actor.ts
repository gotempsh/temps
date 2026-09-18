// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Only host-authored plugin operation records use the plugin actor presentation. */
export function pluginAuditActor(
  operation: string,
  data?: Record<string, unknown>
): { id: string; name: string } | null {
  if (!operation.startsWith('EXTERNAL_PLUGIN_')) return null
  const actor = data?.actor
  if (!actor || typeof actor !== 'object' || Array.isArray(actor)) return null
  const value = actor as Record<string, unknown>
  if (
    value.kind !== 'plugin' ||
    typeof value.id !== 'string' ||
    typeof value.name !== 'string'
  )
    return null
  return { id: value.id, name: value.name }
}
