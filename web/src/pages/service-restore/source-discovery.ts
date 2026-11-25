// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { SourceBackupEntry, SourceBackupIndexResponse } from '@/api/client'

export interface SourceDiscovery {
  orphans: SourceBackupEntry[]
  staleLocations: Set<string>
  errors: string[]
}

export function mergeDiscovery(
  previous: SourceDiscovery | undefined,
  result: SourceBackupIndexResponse
): SourceDiscovery {
  const orphans = result.backups.filter((row) => row.source === 's3_scan')
  const staleLocations = new Set<string>()
  if (result.scan_errors?.length > 0 && previous) {
    const seen = new Set(result.backups.map((row) => row.location))
    for (const row of previous.orphans) {
      if (!seen.has(row.location)) {
        staleLocations.add(row.location)
        orphans.push(row)
      }
    }
  }
  return { orphans, staleLocations, errors: result.scan_errors ?? [] }
}

export function visibleOrphans(
  tracked: SourceBackupEntry[],
  discovery?: SourceDiscovery
) {
  const locations = new Set(tracked.map((row) => row.location))
  return discovery?.orphans.filter((row) => !locations.has(row.location)) ?? []
}
