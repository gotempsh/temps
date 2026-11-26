// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { requireProjectSlug } from '../config/resolve-project.js'
import { getProjectBySlug } from '../api/sdk.gen.js'
import { client, getErrorMessage } from './api-client.js'
import { colors, icons } from '../ui/output.js'
import { isQuietMode } from '../ui/spinner.js'
import { ValidationError } from '../utils/errors.js'

/** A project as the commands that take `--project` need it. */
export interface ProjectRef {
  id: number
  slug: string
}

/**
 * Resolve `--project <slug-or-id>` (or the linked/default project) to its
 * numeric ID. A whole number is the ID itself; anything else is a slug looked
 * up with the API, and so is every project that did not come from the flag:
 * a linked or default project's slug may be all digits. Says where the project
 * came from when it was not the flag, on stderr, so it never mixes with
 * output written to stdout (`temps blob get <path> -o -`).
 */
export async function resolveProjectFlag(flagValue: string | undefined): Promise<ProjectRef> {
  const resolved = await requireProjectSlug(flagValue)
  if (resolved.source !== 'flag') {
    if (!isQuietMode()) console.error(`${icons.info} ${colors.info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)}`)
  }
  if (resolved.source === 'flag' && /^\d+$/.test(resolved.slug)) {
    const id = Number(resolved.slug)
    if (Number.isSafeInteger(id) && id > 0) return { id, slug: resolved.slug }
  }
  const { data, error } = await getProjectBySlug({ client, path: { slug: resolved.slug } })
  if (error || !data) {
    throw new Error(`Could not find project "${resolved.slug}": ${getErrorMessage(error)}`)
  }
  return { id: data.id, slug: data.slug }
}

/** Parse a whole-number flag within `[min, max]`, naming the flag when it is not one. */
export function parseIntegerFlag(raw: string, flag: string, min: number, max: number = Number.MAX_SAFE_INTEGER): number {
  const trimmed = raw.trim()
  const value = Number(trimmed)
  if (!/^-?\d+$/.test(trimmed) || !Number.isSafeInteger(value) || value < min || value > max) {
    const range = max === Number.MAX_SAFE_INTEGER ? `at least ${min}` : `from ${min} to ${max}`
    throw new ValidationError(`${flag} must be a whole number ${range}, got "${raw}"`)
  }
  return value
}
