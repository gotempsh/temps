// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Log storage retains environment IDs; the interface presents their slugs. An
 * ID that can't be resolved keeps its number so two unknown environments
 * never render as the same row.
 */
export function logEnvironmentLabel(
  value: string,
  labels: Record<string, string>
) {
  return (
    labels[value] ??
    (/^\d+$/.test(value) ? `Unknown environment #${value}` : value)
  )
}
