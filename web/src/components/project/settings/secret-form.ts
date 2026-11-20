// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'
import type {
  ProjectSecretResponse,
  UpdateProjectSecretRequest,
} from '@/api/client'
import {
  combinedScope,
  type VariableChangeScope,
} from '@/lib/variable-change-redeploy'

/** Mirror of the server's limit; the server stays authoritative. */
export const MAX_SECRET_BYTES = 1_048_576 // 1 MiB

/** Secret key rules, mirrored from the server for pre-flight feedback. */
export const SECRET_KEY_PATTERN = /^[A-Za-z_][A-Za-z0-9_]{0,254}$/

const utf8Length = (value: string) => new TextEncoder().encode(value).length

/**
 * The edit-secret form. The stored value is write-only and never shown, so
 * `value` starts empty and an empty value means "keep the current value".
 * Scope (environments, preview, compose services) is always sent in full: the
 * update endpoint replaces it, it does not merge.
 */
export const secretEditSchema = z.object({
  value: z.string().refine((value) => utf8Length(value) <= MAX_SECRET_BYTES, {
    message: 'Value exceeds the 1 MiB limit',
  }),
  environment_ids: z.array(z.number().int()),
  include_in_preview: z.boolean(),
  compose_services: z.array(z.string()),
})

export type SecretEditFormValues = z.infer<typeof secretEditSchema>

type StoredSecretScope = Pick<
  ProjectSecretResponse,
  'environments' | 'include_in_preview' | 'compose_services'
>

/**
 * Form state for a secret as it is stored today (value left blank). With no
 * secret selected yet it is the empty, unscoped form.
 */
export function secretEditDefaults(
  secret: StoredSecretScope | null
): SecretEditFormValues {
  if (!secret) {
    return {
      value: '',
      environment_ids: [],
      include_in_preview: false,
      compose_services: [],
    }
  }
  return {
    value: '',
    environment_ids: secret.environments.map((environment) => environment.id),
    include_in_preview: secret.include_in_preview,
    compose_services: [...(secret.compose_services ?? [])],
  }
}

/**
 * Request body for `PUT /projects/{id}/secrets/{id}`. The value is omitted
 * when blank so the stored ciphertext is not touched; it is sent verbatim
 * otherwise (whitespace can be significant in a secret).
 */
export function secretEditToRequest(
  values: SecretEditFormValues
): UpdateProjectSecretRequest {
  const body: UpdateProjectSecretRequest = {
    environment_ids: [...values.environment_ids],
    include_in_preview: values.include_in_preview,
    compose_services: [...values.compose_services],
  }
  if (values.value.length > 0) body.value = values.value
  return body
}

const sameSet = (
  a: readonly (string | number)[],
  b: readonly (string | number)[]
) => a.length === b.length && a.every((item) => b.includes(item))

/** Whether saving would change anything; a no-op save is not offered. */
export function secretEditHasChanges(
  values: SecretEditFormValues,
  secret: StoredSecretScope | null
): boolean {
  if (!secret) return false
  const current = secretEditDefaults(secret)
  return (
    values.value.length > 0 ||
    values.include_in_preview !== current.include_in_preview ||
    !sameSet(values.environment_ids, current.environment_ids) ||
    !sameSet(values.compose_services, current.compose_services)
  )
}

/**
 * Environments whose running deployments are affected by an edit: those the
 * secret was in and those it is now in. An unscoped secret is mounted in
 * every environment.
 */
export function secretEditChangeScope(
  before: readonly number[],
  after: readonly number[]
): VariableChangeScope {
  const toScope = (ids: readonly number[]): VariableChangeScope =>
    ids.length === 0 ? 'all' : ids
  return combinedScope([toScope(before), toScope(after)])
}

/** Compose service names the project last synced from its compose file.
 *  Empty for non-Compose presets, which deploy a single container. */
export function composeServiceNamesFor(project: {
  preset?: string | null
  preset_config?: unknown
}): string[] {
  const cfg = (project.preset_config ?? {}) as Record<string, unknown>
  const services = (cfg.composeServices ??
    cfg.compose_services ??
    []) as Array<{
    name?: string
  }>
  return Array.from(
    new Set(
      services
        .map((service) => service?.name)
        .filter((name): name is string => typeof name === 'string' && !!name)
    )
  ).sort()
}
