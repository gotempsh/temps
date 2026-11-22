// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import type { ActivityOperation, ActivityStatus } from '@/api/client/types.gen'
import {
  activityGate,
  secondsUntil,
  type ActivityGate,
} from './activity-request-state'

/** Current time, ticking once a second while `active`. */
function useNow(active: boolean): number {
  const [now, setNow] = useState(Date.now)
  useEffect(() => {
    if (!active) return
    const tick = () => setNow(Date.now())
    // Catch up at once: the clock stood still while nothing was counting.
    const catchUp = setTimeout(tick, 0)
    const timer = setInterval(tick, 1000)
    return () => {
      clearTimeout(catchUp)
      clearInterval(timer)
    }
  }, [active])
  return now
}

export interface ActivityGateState extends ActivityGate {
  now: number
  cooldownSecs: number
  /** True while the action must not be started again. */
  blocked: boolean
}

/** Whether `operation` can start, kept current to the second. */
export function useActivityGate(
  status: Pick<ActivityStatus, 'in_progress' | 'cooldowns'> | undefined,
  operation: ActivityOperation,
  statusUpdatedAt: number
): ActivityGateState {
  const gate = activityGate(status, operation, statusUpdatedAt)
  const now = useNow(gate.running != null || gate.cooldownUntil != null)
  const cooldownSecs = secondsUntil(gate.cooldownUntil, now)
  return {
    ...gate,
    now,
    cooldownSecs,
    blocked: gate.running != null || cooldownSecs > 0,
  }
}
