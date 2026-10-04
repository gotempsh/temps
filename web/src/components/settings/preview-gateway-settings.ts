// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { PreviewGatewaySettingsResponse } from '@/api/client'

/**
 * The container name to send with a settings save, or `undefined` to keep
 * the saved one. A blank field means the default name. The server checks
 * the name and renames the gateway, so it is sent only when it would change.
 */
export function containerNameToSave(
  input: string,
  saved: Pick<
    PreviewGatewaySettingsResponse,
    'container_name' | 'default_container_name'
  > | null
): string | undefined {
  if (!saved) return undefined
  const requested = input.trim() || saved.default_container_name
  return requested === saved.container_name ? undefined : requested
}
