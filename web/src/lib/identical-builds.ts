// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { IdenticalBuildsResponse } from '@/api/client'

/** What coordination did in the last 30 days, in words; `null` when idle. */
export function describeActivity(
  activity: IdenticalBuildsResponse['activity']
): string | null {
  const parts: string[] = []
  if (activity.reused > 0)
    parts.push(
      `${activity.reused} deployment${activity.reused === 1 ? '' : 's'} reused an identical build's image`
    )
  if (activity.built > 0)
    parts.push(
      `${activity.built} coordinated build${activity.built === 1 ? '' : 's'} ran`
    )
  if (activity.waiting > 0) parts.push(`${activity.waiting} waiting now`)
  if (parts.length === 0) return null
  return `Last 30 days: ${parts.join(', ')}.`
}
