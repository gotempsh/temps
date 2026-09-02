// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export function telemetryFreshnessSummary(
  engine: string,
  metricNames: Iterable<string>,
  lastReceivedRelative: string | null
): string | null {
  if (engine !== 'rustfs') {
    return lastReceivedRelative ? `last received ${lastReceivedRelative}` : null
  }

  const names = Array.from(metricNames)
  const hasApplicationMetrics = names.some((name) => name.startsWith('rustfs_'))

  if (hasApplicationMetrics) {
    return lastReceivedRelative
      ? `RustFS application metrics present · telemetry received ${lastReceivedRelative}`
      : 'RustFS application metrics present'
  }

  if (names.length > 0) {
    return 'Container telemetry active · RustFS application metrics not received'
  }

  return lastReceivedRelative
    ? `Telemetry received ${lastReceivedRelative} · RustFS application metrics not received`
    : 'RustFS application metrics not received'
}
