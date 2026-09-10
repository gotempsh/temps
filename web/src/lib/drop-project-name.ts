// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { customAlphabet } from 'nanoid'
import slugify from 'slugify'

const randomDropId = customAlphabet('abcdefghijklmnopqrstuvwxyz0123456789', 8)
const MAX_PROJECT_NAME_LENGTH = 40

/**
 * Convert user- or filename-derived text to the same DNS-safe shape used by
 * project URLs. An empty result is intentionally preserved so callers can
 * distinguish "still editing" from "needs a generated name".
 */
export function normalizeDropProjectName(value: string): string {
  return slugify(value, {
    lower: true,
    strict: true,
    trim: true,
  })
    .slice(0, MAX_PROJECT_NAME_LENGTH)
    .replace(/-+$/g, '')
}

/** Return a valid project slug, generating one when the source has no name. */
export function ensureDropProjectName(
  value: string,
  createId: () => string = randomDropId
): string {
  return normalizeDropProjectName(value) || `drop-${createId()}`
}
