// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface ServiceCreationDefaults {
  name: string
  parameters: Record<string, string | number | boolean | null>
  topology: 'standalone' | 'cluster'
  node_id: number | null
}

/**
 * Read the backend-owned defaults embedded in a service parameter schema.
 * The same response is consumed by the classic form and the AI read tool.
 */
export function serviceCreationDefaults(
  schema: unknown
): ServiceCreationDefaults | null {
  if (!schema || typeof schema !== 'object') return null
  const defaults = (schema as Record<string, unknown>)[
    'x-temps-creation-defaults'
  ]
  if (!defaults || typeof defaults !== 'object') return null

  const candidate = defaults as Record<string, unknown>
  if (
    typeof candidate.name !== 'string' ||
    !candidate.parameters ||
    typeof candidate.parameters !== 'object' ||
    (candidate.topology !== 'standalone' && candidate.topology !== 'cluster') ||
    (candidate.node_id !== null && typeof candidate.node_id !== 'number')
  ) {
    return null
  }

  return candidate as unknown as ServiceCreationDefaults
}
