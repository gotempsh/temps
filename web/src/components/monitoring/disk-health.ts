// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  DriveHealthRow,
  NodeDiskHealthResponse,
} from '@/api/client/types.gen'
import { formatBytesDecimal } from '@/lib/server-monitoring'

/** Wear above this is flagged; matches the default "Drive wear" alert rule. */
export const WEAR_WARN_PERCENT = 80

export type DiskHealthTone = 'good' | 'warn' | 'poor' | 'unknown'

export function driveTone(d: DriveHealthRow): DiskHealthTone {
  if ((d.media_errors ?? 0) > 0) return 'poor'
  if (d.wear_percent != null && d.wear_percent > WEAR_WARN_PERCENT)
    return 'warn'
  if (d.wear_percent == null && d.media_errors == null) return 'unknown'
  return 'good'
}

export function overallTone(h: NodeDiskHealthResponse): DiskHealthTone {
  if (h.raid_arrays.some((a) => a.degraded)) return 'poor'
  const tones = h.drives.map(driveTone)
  if (tones.includes('poor')) return 'poor'
  if (tones.includes('warn')) return 'warn'
  if (h.raid_arrays.length === 0 && h.drives.length === 0) return 'unknown'
  return 'good'
}

export function summarize(h: NodeDiskHealthResponse): string {
  if (!h.reported) return 'Not reported'
  const degraded = h.raid_arrays.filter((a) => a.degraded).length
  const parts: string[] = []
  if (h.raid_arrays.length > 0) {
    parts.push(
      degraded > 0
        ? `${degraded} of ${h.raid_arrays.length} RAID array${h.raid_arrays.length === 1 ? '' : 's'} degraded`
        : `${h.raid_arrays.length} RAID array${h.raid_arrays.length === 1 ? '' : 's'} healthy`
    )
  }
  if (h.drives_detected > 0) {
    parts.push(
      `${h.drives_read} of ${h.drives_detected} drive${h.drives_detected === 1 ? '' : 's'} read`
    )
  }
  return parts.length > 0 ? parts.join(' · ') : 'No arrays or drives found'
}

export function describeDrive(d: DriveHealthRow): string {
  const parts: string[] = []
  if (d.wear_percent != null) parts.push(`${Math.round(d.wear_percent)}% worn`)
  if (d.media_errors != null) {
    parts.push(
      `${d.media_errors.toLocaleString()} media error${d.media_errors === 1 ? '' : 's'}`
    )
  }
  if (d.bytes_written != null) {
    parts.push(`${formatBytesDecimal(d.bytes_written)} written`)
  }
  return parts.length > 0 ? parts.join(' · ') : 'No SMART data'
}
