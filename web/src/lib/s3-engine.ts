// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

const S3_SERVICE_TYPES = ['s3', 'blob', 'rustfs', 'minio']

/**
 * Storage engine behind an S3-compatible service, or `null` for other service
 * types. Services created before engines were selectable have no `backend`
 * and run RustFS; legacy MinIO services keep their own type.
 */
export function s3EngineLabel(
  serviceType: string,
  parameters: Record<string, unknown> | null | undefined
): string | null {
  if (!S3_SERVICE_TYPES.includes(serviceType)) return null
  if (serviceType === 'minio') return 'MinIO (legacy)'
  const backend = String(parameters?.backend ?? '')
    .trim()
    .toLowerCase()
  switch (backend) {
    case 'garage':
      return 'Garage'
    case 'minio':
      return 'MinIO (legacy)'
    default:
      return 'RustFS'
  }
}

/** Whether data can be copied into/out of this service by the S3 migration. */
export function s3MigrationSupported(
  serviceType: string,
  parameters: Record<string, unknown> | null | undefined
): boolean {
  const engine = s3EngineLabel(serviceType, parameters)
  return engine === 'RustFS' || engine === 'Garage'
}
