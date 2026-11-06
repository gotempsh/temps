// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The stop timeout: how long a replaced deployment's containers get between
 * SIGTERM and SIGKILL. The server resolves the environment value, then the
 * project value, then this default, and refuses anything outside the range.
 */
export const DEFAULT_STOP_TIMEOUT_SECONDS = 10
export const MIN_STOP_TIMEOUT_SECONDS = 1
export const MAX_STOP_TIMEOUT_SECONDS = 3600

/**
 * Why a typed stop timeout cannot be saved, or `null` when it can. Blank is
 * valid: it means "use the default" (project) or "follow the project"
 * (environment).
 */
export function stopTimeoutProblem(value: string | undefined): string | null {
  const trimmed = value?.trim() ?? ''
  if (trimmed === '') return null
  const seconds = Number(trimmed)
  if (
    !/^\d+$/.test(trimmed) ||
    seconds < MIN_STOP_TIMEOUT_SECONDS ||
    seconds > MAX_STOP_TIMEOUT_SECONDS
  ) {
    return `Enter a whole number of seconds between ${MIN_STOP_TIMEOUT_SECONDS} and ${MAX_STOP_TIMEOUT_SECONDS}`
  }
  return null
}

/** The typed value as the API takes it: `null` for blank. */
export function stopTimeoutSeconds(value: string | undefined): number | null {
  const trimmed = value?.trim() ?? ''
  return trimmed === '' ? null : Number(trimmed)
}
