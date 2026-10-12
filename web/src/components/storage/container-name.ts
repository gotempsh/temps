// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as z from 'zod'

/** Longest container name the API accepts (a DNS label). */
export const MAX_CONTAINER_NAME_LENGTH = 63

/**
 * Why `name` cannot be the container of a managed service whose containers
 * must start with `prefix`, or `null` when it can. Mirrors the server's
 * validation so a typo is caught before the request; the server stays the
 * authority. An empty name is valid: it restores the per-instance default.
 */
export function containerNameError(
  name: string,
  prefix: string
): string | null {
  const trimmed = name.trim()
  if (trimmed === '') return null
  if (trimmed.length > MAX_CONTAINER_NAME_LENGTH) {
    return `Use at most ${MAX_CONTAINER_NAME_LENGTH} characters.`
  }
  if (!trimmed.startsWith(prefix) || trimmed.length === prefix.length) {
    return `The name must start with “${prefix}” followed by at least one character.`
  }
  if (!/^[A-Za-z0-9_-]+$/.test(trimmed)) {
    return 'Use only letters, digits, “-” and “_”.'
  }
  return null
}

/** Form schema for the container-name field of a service whose names start with `prefix`. */
export function containerNameSchema(prefix: string) {
  return z.object({
    container_name: z.string().superRefine((value, context) => {
      const message = containerNameError(value, prefix)
      if (message) context.addIssue({ code: 'custom', message })
    }),
  })
}

export type ContainerNameFormValues = z.infer<
  ReturnType<typeof containerNameSchema>
>
