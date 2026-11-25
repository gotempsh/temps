// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Hand-written helpers for S3 source endpoints that are not yet reflected in the
 * generated OpenAPI client. Once `bun run openapi-ts` is re-run against a server
 * that exposes these endpoints, this file can be deleted and the generated SDK
 * used directly.
 *
 * TODO(sdk-regen): replace with generated SDK helpers for
 *   - POST /backups/s3-sources/{id}/set-default
 *   - POST /backups/s3-sources/{id}/test
 *   - POST /backups/s3-sources/test
 * once these endpoints are included in the OpenAPI spec / generated client.
 */

export interface S3ConnectionTestResult {
  ok: boolean
  message: string
}

export interface TestS3ConnectionPreviewBody {
  name: string
  bucket_name: string
  bucket_path: string
  access_key_id: string
  secret_key: string
  region: string
  endpoint?: string | null
  force_path_style?: boolean | null
  is_default?: boolean | null
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

export async function setDefaultS3Source(id: number) {
  const response = await fetch(`/api/backups/s3-sources/${id}/set-default`, {
    method: 'POST',
    credentials: 'include',
  })
  return readJsonOrThrow<{ id: number; is_default: boolean; name: string }>(
    response
  )
}

export async function testS3SourceConnection(
  id: number
): Promise<S3ConnectionTestResult> {
  const response = await fetch(`/api/backups/s3-sources/${id}/test`, {
    method: 'POST',
    credentials: 'include',
  })
  return readJsonOrThrow<S3ConnectionTestResult>(response)
}

export async function testS3ConnectionPreview(
  body: TestS3ConnectionPreviewBody
): Promise<S3ConnectionTestResult> {
  const response = await fetch(`/api/backups/s3-sources/test`, {
    method: 'POST',
    credentials: 'include',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  return readJsonOrThrow<S3ConnectionTestResult>(response)
}
