// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'

/** Blank restores image defaults; JSON preserves argument boundaries exactly. */
export function parseRuntimeArguments(value: string): string[] | null {
  if (!value.trim()) return null
  const parsed: unknown = JSON.parse(value)
  if (
    !Array.isArray(parsed) ||
    !parsed.every((part) => typeof part === 'string')
  ) {
    throw new Error('Enter a JSON array of strings')
  }
  return parsed
}

function runtimeArgumentsSchema(allowEmpty: boolean) {
  return z.string().superRefine((value, context) => {
    try {
      const args = parseRuntimeArguments(value)
      if (args === null) return
      if ((!allowEmpty && args.length === 0) || args.length > 64) {
        throw new Error(
          allowEmpty
            ? 'Use at most 64 arguments'
            : 'Use 1–64 arguments, or leave blank for the image default'
        )
      }
      if (
        args.some(
          (part) =>
            new TextEncoder().encode(part).length > 1024 || part.includes('\0')
        )
      ) {
        throw new Error(
          'Each argument must be at most 1024 bytes and contain no null characters'
        )
      }
      if (args.length > 0 && !args[0].trim()) {
        throw new Error('The first argument cannot be blank')
      }
    } catch (error) {
      context.addIssue({
        code: 'custom',
        message:
          error instanceof SyntaxError
            ? 'Enter a JSON array, for example ["python", "-m", "worker"]'
            : (error as Error).message,
      })
    }
  })
}

export const containerRuntimeSchema = z.object({
  command: runtimeArgumentsSchema(false),
  entrypoint: runtimeArgumentsSchema(true),
})
