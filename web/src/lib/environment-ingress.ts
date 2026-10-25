// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getIngressSettings as getIngressSettingsOperation,
  updateIngressSettings as updateIngressSettingsOperation,
} from '@/api/client'
import type {
  EnvironmentIngressMode as IngressMode,
  IngressSettingsResponse,
  UpdateIngressSettingsRequest,
} from '@/api/client'

/** Which evaluator decides an environment's requests (ADR-055 §11). */
export type { IngressMode }

/** An environment's ingress settings, as the server returns them. */
export type IngressSettings = IngressSettingsResponse

/** A partial update: a field left out keeps its current value. */
export type IngressSettingsUpdate = UpdateIngressSettingsRequest

export function ingressSettingsQueryKey(
  projectId: number,
  environmentId: number
) {
  return ['environment-ingress', projectId, environmentId] as const
}

export async function getIngressSettings(
  projectId: number,
  environmentId: number
): Promise<IngressSettings> {
  const { data } = await getIngressSettingsOperation({
    path: { project_id: projectId, env_id: environmentId },
    throwOnError: true,
  })
  return data
}

export async function updateIngressSettings(
  projectId: number,
  environmentId: number,
  update: IngressSettingsUpdate
): Promise<IngressSettings> {
  const { data } = await updateIngressSettingsOperation({
    path: { project_id: projectId, env_id: environmentId },
    body: update,
    throwOnError: true,
  })
  return data
}

/**
 * The sentence that tells the operator what a saved setting does right
 * now, since a required login only applies once the endpoint decides.
 */
export function ingressStatusSummary(settings: IngressSettings): string {
  if (settings.mode === 'legacy') {
    return settings.require_login
      ? 'The existing access rules decide. The Temps login is saved but only applies in Enforce mode.'
      : 'The existing access rules decide.'
  }
  if (settings.mode === 'shadow') {
    return settings.require_login
      ? 'The existing access rules decide; the compiled endpoint, including the Temps login, is evaluated alongside and disagreements are counted.'
      : 'The existing access rules decide; the compiled endpoint is evaluated alongside and disagreements are counted.'
  }
  return settings.login_enforced
    ? 'The compiled endpoint decides, and visitors must sign in to Temps.'
    : 'The compiled endpoint decides.'
}
