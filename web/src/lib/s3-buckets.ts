// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Bucket names a managed S3 service creates for every environment that links
 * it. The rules mirror `crates/temps-providers/src/externalsvc/managed_s3.rs`
 * (`normalize_bucket_names`); the server stays the authority, this only gives
 * the operator the same answer before they submit.
 */
export const MIN_S3_BUCKET_LENGTH = 3
export const MAX_S3_BUCKET_LENGTH = 32
export const MAX_S3_BUCKETS = 16

/** Split free text on commas, whitespace and newlines; lower-case; drop duplicates. */
export function parseBucketInput(raw: string): string[] {
  const names: string[] = []
  for (const part of raw.split(/[\s,]+/)) {
    const name = part.trim().toLowerCase()
    if (name && !names.includes(name)) names.push(name)
  }
  return names
}

/** The first problem with `names`, or `null` when they are all acceptable. */
export function bucketNamesError(names: string[]): string | null {
  if (names.length > MAX_S3_BUCKETS) {
    return `A service can declare at most ${MAX_S3_BUCKETS} buckets (got ${names.length}).`
  }
  for (const name of names) {
    if (
      name.length < MIN_S3_BUCKET_LENGTH ||
      name.length > MAX_S3_BUCKET_LENGTH
    ) {
      return `Bucket "${name}" must be ${MIN_S3_BUCKET_LENGTH}-${MAX_S3_BUCKET_LENGTH} characters long.`
    }
    if (!/^[a-z0-9-]+$/.test(name)) {
      return `Bucket "${name}" may only contain lower-case letters, digits and hyphens.`
    }
    if (!/^[a-z0-9]/.test(name) || !/[a-z0-9]$/.test(name)) {
      return `Bucket "${name}" must start and end with a letter or digit.`
    }
  }
  return null
}

/** Environment variable carrying a bucket's name, e.g. `uploads` -> `S3_BUCKET_UPLOADS`. */
export function bucketEnvVarName(bucket: string): string {
  return `S3_BUCKET_${bucket.toUpperCase().replace(/-/g, '_')}`
}

/** Value for the `buckets` creation parameter; `undefined` when none are declared. */
export function bucketsParameter(names: string[]): string | undefined {
  return names.length > 0 ? names.join(',') : undefined
}
