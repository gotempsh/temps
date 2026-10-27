// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Mirrors `ContainerLogSettings::MAX_INGEST_*` on the server. */
export const MAX_LINES_PER_SEC = 1_000_000
export const MAX_KIB_PER_SEC = 1_048_576

/**
 * A budget field as typed: blank inherits the platform budget (`null`), a
 * whole number in `0..=max` is an override (`0` = unlimited), anything else
 * is invalid (`undefined`).
 */
export function parseBudget(
  raw: string,
  max: number
): number | null | undefined {
  const trimmed = raw.trim()
  if (trimmed === '') return null
  const value = Number(trimmed)
  if (!Number.isInteger(value) || value < 0 || value > max) return undefined
  return value
}

export function budgetInputValue(value: number | null | undefined): string {
  return value == null ? '' : String(value)
}

/**
 * What a field means once saved, e.g. "Platform default: 500 lines/s",
 * "Unlimited", "2,000 lines/s (own budget)".
 */
export function describeBudget(
  value: number | null | undefined,
  platform: number | undefined,
  unit: string
): string {
  if (value == null) {
    if (platform === undefined) return 'Platform default'
    return platform === 0
      ? 'Platform default: unlimited'
      : `Platform default: ${platform.toLocaleString()} ${unit}`
  }
  if (value === 0) return 'Unlimited'
  return `${value.toLocaleString()} ${unit} (own budget)`
}
