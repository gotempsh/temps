// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ClusterMemberHealthResponse,
  ClusterProtectionResponse,
} from '@/api/client'

/**
 * Pure helpers for the cluster Members table. The data comes from the
 * generated SDK (`getClusterHealthOptions`).
 */
export type ClusterMemberHealth = ClusterMemberHealthResponse

/** How the panel presents the cluster's failover protection. */
export interface ProtectionPresentation {
  tone: 'protected' | 'not-protected' | 'unknown'
  title: string
  /** The server's detail without the title it already starts with. */
  body: string
}

function withoutPrefix(detail: string, title: string): string {
  if (!detail.startsWith(`${title}: `)) return detail
  const rest = detail.slice(title.length + 2)
  return rest.charAt(0).toUpperCase() + rest.slice(1)
}

/**
 * `not_protected` is the state a new cluster is in while its first standby
 * copies the primary: it accepts SQL and every member runs, yet a primary
 * failure could not fail over. Say so instead of looking ready.
 */
export function describeProtection(
  protection: ClusterProtectionResponse
): ProtectionPresentation {
  switch (protection.state) {
    case 'protected':
      return {
        tone: 'protected',
        title: 'Protected by a standby',
        body: protection.detail,
      }
    case 'not_protected': {
      const title = protection.detail.startsWith('Not yet protected')
        ? 'Not yet protected'
        : 'Not protected'
      return {
        tone: 'not-protected',
        title,
        body: withoutPrefix(protection.detail, title),
      }
    }
    case 'unknown':
      return {
        tone: 'unknown',
        title: 'Protection unknown',
        body: protection.detail,
      }
  }
}

/**
 * Bucket a member into one of four health states for the row's left dot.
 * Order of precedence (most-severe wins):
 * - unreachable: monitor hasn't heard from the node in >30s OR health<0.
 *                Overrides reported_state — a stale "primary" badge means
 *                nothing if the node isn't actually responding.
 * - down: state is terminal/non-data (draining, demoted, dropped, …).
 * - degraded: state is recoverable (catchingup, wait_primary, apply_settings)
 *             or lag is high.
 * - healthy: in a known-good state and reporting recently.
 */
export type MemberHealthBucket = 'unreachable' | 'down' | 'degraded' | 'healthy'

const STALE_REPORT_SECONDS = 30

export function bucketMemberHealth(m: ClusterMemberHealth): MemberHealthBucket {
  // Liveness wins. A node whose reporttime is stale is functionally down
  // even if its last self-report claimed primary. Defensive against
  // older backends that omit `health` / `seconds_since_report` — coerce
  // missing fields to "healthy and just-reported" so the panel doesn't
  // wrongly mark every node unreachable when the backend is mid-restart.
  const health = m.health ?? 1
  const sinceReport = m.seconds_since_report ?? 0
  if (health < 0 || sinceReport > STALE_REPORT_SECONDS) {
    return 'unreachable'
  }

  switch (m.reported_state) {
    case 'primary':
    case 'single':
    case 'secondary':
      if (m.replay_lag_ms != null && m.replay_lag_ms > 5000) {
        return 'degraded'
      }
      return 'healthy'
    case 'catchingup':
    case 'apply_settings':
    case 'wait_primary':
      return 'degraded'
    default:
      // draining, demoted, dropped, …
      return 'down'
  }
}

/** True when the monitor is actively trying to move this node to a different state. */
export function isTransitioning(m: ClusterMemberHealth): boolean {
  // Defensive: an older backend might not return goal_state at all.
  // Treat the missing case as "not transitioning" so the UI doesn't try
  // to render an arrow with `undefined`.
  if (!m.goal_state) return false
  return m.goal_state !== m.reported_state
}
