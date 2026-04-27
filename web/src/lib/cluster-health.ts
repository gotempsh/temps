/**
 * Hand-written helper for the per-cluster health endpoint, pending OpenAPI
 * SDK regeneration.
 *
 * TODO(sdk-regen): replace with generated SDK helper for
 *   - GET /external-services/{id}/cluster-health
 * once `bun run openapi-ts` is re-run against a server that includes the
 * endpoint.
 */

export interface ClusterMemberHealth {
  nodename: string
  nodehost: string
  nodeport: number
  /** What the node *last told the monitor* it was. Stale during outages. */
  reported_state: string
  /** What the monitor *wants* this node to be. Different mid-transition. */
  goal_state: string
  /** pg_auto_failover liveness: 1 healthy, 0 unknown (no recent report), -1 unhealthy. */
  health: number
  /** Wall-clock seconds since the node last reported in. */
  seconds_since_report: number
  candidate_priority: number
  replication_quorum: boolean
  /** sync | quorum | async — null on the primary itself. */
  sync_state: string | null
  /** Replay lag from pg_stat_replication, in milliseconds. */
  replay_lag_ms: number | null
}

export interface ClusterHealthReport {
  /** ISO-8601 wall-clock when the report was generated. */
  checked_at: string
  /** Round-trip to query the monitor (ms). */
  monitor_response_ms: number
  members: ClusterMemberHealth[]
  /** Set when the monitor itself was unreachable. */
  monitor_error?: string | null
}

async function readJsonOrThrow<T>(response: Response): Promise<T> {
  if (!response.ok) {
    let detail = `${response.status} ${response.statusText}`
    try {
      const body = await response.json()
      detail = (body && (body.detail || body.title)) || detail
    } catch {
      // fall through with statusText
    }
    throw new Error(detail)
  }
  return (await response.json()) as T
}

export async function getClusterHealth(
  serviceId: number,
): Promise<ClusterHealthReport> {
  const response = await fetch(
    `/api/external-services/${serviceId}/cluster-health`,
    { credentials: 'include' },
  )
  return readJsonOrThrow<ClusterHealthReport>(response)
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
