// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Argument parsing shared by the `nodes` commands (unit tested).

/**
 * A positive integer id, written only with digits: `12abc`, `1.5`, `-3` and
 * `0x10` are refused rather than read as a different id.
 */
export function parseId(value: string): number | null {
  const trimmed = value.trim()
  if (!/^\d+$/.test(trimmed)) return null
  const id = Number(trimmed)
  return Number.isSafeInteger(id) && id > 0 ? id : null
}

/**
 * Commander parser for a port option: digits only, so `22abc` becomes NaN
 * and fails `validPort` instead of being read as 22.
 */
export function parsePortOption(value: string): number {
  const trimmed = value.trim()
  return /^\d+$/.test(trimmed) ? Number(trimmed) : Number.NaN
}

/** An unset port, or a whole number from 1 to 65535. */
export function validPort(port: number | undefined): boolean {
  return port === undefined || (Number.isInteger(port) && port >= 1 && port <= 65535)
}
