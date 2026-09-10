// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Hand-written helpers for git-provider endpoints not yet reflected in the
 * generated OpenAPI client. Once `bun run openapi-ts` is re-run against a
 * server that exposes these endpoints, switch to the generated SDK.
 *
 * TODO(sdk-regen): replace with generated SDK helpers for
 *   - PATCH /git-providers/{id}/credentials
 * once these endpoints are included in the OpenAPI spec / generated client.
 */

export interface UpdateProviderCredentialsBody {
  client_id?: string
  client_secret?: string
  app_id?: string
  app_secret?: string
  private_key?: string
  webhook_secret?: string
  redirect_uri?: string
  token?: string
}

export interface ProviderResponse {
  id: number
  name: string
  provider_type: string
  base_url: string | null
  auth_method: string
  is_active: boolean
  is_default: boolean
  created_at: string
  updated_at: string
}

async function readJsonOrThrow<T>(response: Response): Promise<T> {
  if (!response.ok) {
    let detail = response.statusText
    try {
      const body = (await response.json()) as {
        detail?: string
        title?: string
      }
      detail = body.detail || body.title || detail
    } catch {
      // fall through with statusText
    }
    throw new Error(detail)
  }
  return (await response.json()) as T
}

export async function updateGitProviderCredentials(
  providerId: number,
  body: UpdateProviderCredentialsBody,
): Promise<ProviderResponse> {
  const response = await fetch(`/api/git-providers/${providerId}/credentials`, {
    method: 'PATCH',
    credentials: 'include',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  return readJsonOrThrow<ProviderResponse>(response)
}
