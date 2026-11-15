// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { SecretResponse as AgentSecret } from '@/api/client/types.gen'

/** "Instance-wide" or the project a secret is scoped to. */
export function secretScopeLabel(secret: AgentSecret): string {
  return secret.project_id == null
    ? 'Instance-wide'
    : `Project #${secret.project_id}`
}

/**
 * The scope selector for a request about `secret`. A name can exist both
 * instance-wide and per project, so a request must always name the scope of
 * the row the user picked -- omitting it targets the instance-wide secret.
 */
export function secretScopeQuery(
  secret: AgentSecret
): { project_id: number } | undefined {
  return secret.project_id == null
    ? undefined
    : { project_id: secret.project_id }
}
