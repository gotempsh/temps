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
 *   - GET  /backups/s3-sources/{id}/backups?include_s3_scan=true (scan variant)
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
      const body = (await response.json()) as { detail?: string; title?: string }
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
  return readJsonOrThrow<{ id: number; is_default: boolean; name: string }>(response)
}

export async function testS3SourceConnection(
  id: number,
): Promise<S3ConnectionTestResult> {
  const response = await fetch(`/api/backups/s3-sources/${id}/test`, {
    method: 'POST',
    credentials: 'include',
  })
  return readJsonOrThrow<S3ConnectionTestResult>(response)
}

/**
 * Fetch backups for an S3 source while requesting the full S3 bucket scan.
 * This is the slow path — may take 5-30 s on OVH and similar endpoints.
 * Use only when the user explicitly requests it (e.g. "Discover orphan backups").
 *
 * TODO(sdk-regen): replace with the generated SDK helper once
 * `listSourceBackupsOptions` supports `include_s3_scan` query param.
 */
export async function listSourceBackupsWithScan(id: number): Promise<{
  backups: Array<Record<string, unknown>>
  last_updated: string
}> {
  const response = await fetch(
    `/api/backups/s3-sources/${id}/backups?include_s3_scan=true`,
    { credentials: 'include' },
  )
  return readJsonOrThrow(response)
}

export async function testS3ConnectionPreview(
  body: TestS3ConnectionPreviewBody,
): Promise<S3ConnectionTestResult> {
  const response = await fetch(`/api/backups/s3-sources/test`, {
    method: 'POST',
    credentials: 'include',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  return readJsonOrThrow<S3ConnectionTestResult>(response)
}
