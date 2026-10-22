// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { client } from '@/api/client/client.gen'

/** Which evaluator decides an environment's requests (ADR-055 §11). */
export type IngressMode = 'legacy' | 'shadow' | 'enforce'

/** Mirrors `IngressSettingsResponse` on the server. */
export interface IngressSettings {
  project_id: number
  environment_id: number
  mode: IngressMode
  require_login: boolean
  login_permission: string | null
  login_enforced: boolean
}

/** Mirrors `UpdateIngressSettingsRequest`. */
export interface IngressSettingsUpdate {
  mode?: IngressMode
  require_login?: boolean
}

const BEARER_SECURITY = [{ scheme: 'bearer', type: 'http' }] as const

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
  const { data } = await client.get<IngressSettings, unknown, true>({
    security: [...BEARER_SECURITY],
    url: `/projects/${projectId}/environments/${environmentId}/ingress`,
    throwOnError: true,
  })
  return data
}

export async function updateIngressSettings(
  projectId: number,
  environmentId: number,
  update: IngressSettingsUpdate
): Promise<IngressSettings> {
  const { data } = await client.put<IngressSettings, unknown, true>({
    security: [...BEARER_SECURITY],
    url: `/projects/${projectId}/environments/${environmentId}/ingress`,
    body: update,
    headers: { 'Content-Type': 'application/json' },
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
