// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { EnvironmentResponse } from '@/api/client'

/** Environment pointers are authoritative; the newest build may not be live. */
export function projectDeploymentStatus(
  environments: Pick<EnvironmentResponse, 'current_deployment_id'>[] | undefined
): 'Deployed' | 'Not deployed' | undefined {
  if (environments === undefined) return undefined
  return environments.some(
    (environment) => environment.current_deployment_id != null
  )
    ? 'Deployed'
    : 'Not deployed'
}
