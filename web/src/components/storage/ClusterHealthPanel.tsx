/**
 * Per-cluster Members table for the Postgres HA cluster detail page.
 *
 * Polls `GET /external-services/{id}/cluster-health` every 5 seconds via
 * Tanstack Query. Each row shows a member's reported state, sync state,
 * and replay lag — all read from pg_auto_failover's monitor + the current
 * primary's `pg_stat_replication`. The monitor is the source of truth so
 * we don't need to probe each member ourselves.
 *
 * If the monitor is briefly unreachable (mid-failover), the panel shows a
 * banner instead of the table. No fallback to stale data — we'd rather
 * show "monitor unreachable" than misleading green dots.
 */

import { useQuery } from '@tanstack/react-query'
import { Loader2, RefreshCw } from 'lucide-react'

import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { TimeAgo } from '@/components/utils/TimeAgo'
import {
  bucketMemberHealth,
  ClusterMemberHealth,
  getClusterHealth,
  isTransitioning,
  type ClusterHealthReport,
  type MemberHealthBucket,
} from '@/lib/cluster-health'

interface Props {
  serviceId: number
  /**
   * Polling interval in ms. 5 s matches the role reconciler's tick — any
   * shorter just hammers the monitor with no extra signal. Caller can
   * disable polling for off-screen tabs by passing `false`.
   */
  refetchInterval?: number | false
}

const DOT_CLASS: Record<MemberHealthBucket, string> = {
  healthy: 'bg-emerald-500',
  degraded: 'bg-amber-500',
  down: 'bg-rose-500',
  unreachable: 'bg-zinc-500',
}

const DOT_LABEL: Record<MemberHealthBucket, string> = {
  healthy: 'Healthy',
  degraded: 'Degraded',
  down: 'Down',
  unreachable: 'Unreachable',
}

/** Compact "no report for X" summary for a stale row. */
function formatLastSeen(seconds: number): string {
  if (seconds < 60) return `${seconds}s ago`
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`
  return `${Math.floor(seconds / 3600)}h ago`
}

/** Format `replay_lag_ms` for the "Lag" column. */
function formatLag(lagMs: number | null): string {
  if (lagMs == null) return '—'
  if (lagMs < 1) return '<1 ms'
  if (lagMs < 1000) return `${Math.round(lagMs)} ms`
  return `${(lagMs / 1000).toFixed(1)} s`
}

function syncStateBadgeVariant(
  syncState: string | null,
): 'default' | 'secondary' | 'outline' {
  switch (syncState) {
    case 'sync':
    case 'quorum':
      return 'default'
    case 'async':
      return 'outline'
    default:
      return 'secondary'
  }
}

function reportedStateBadgeVariant(
  state: string,
): 'default' | 'secondary' | 'outline' | 'destructive' {
  switch (state) {
    case 'primary':
    case 'single':
      return 'default'
    case 'secondary':
      return 'secondary'
    case 'catchingup':
    case 'apply_settings':
    case 'wait_primary':
      return 'outline'
    default:
      return 'destructive'
  }
}

export function ClusterHealthPanel({
  serviceId,
  refetchInterval = 5000,
}: Props) {
  const {
    data: report,
    error,
    isLoading,
    isFetching,
    refetch,
    dataUpdatedAt,
  } = useQuery<ClusterHealthReport, Error>({
    queryKey: ['cluster-health', serviceId],
    queryFn: () => getClusterHealth(serviceId),
    // Poll every `refetchInterval` ms regardless of focus. The cluster's
    // role can flip mid-failover and ops needs to see it immediately.
    refetchInterval,
    refetchIntervalInBackground: true,
    refetchOnWindowFocus: true,
    // Treat data as stale immediately so refetches always re-render
    // (otherwise Tanstack can short-circuit identical-looking responses).
    staleTime: 0,
  })

  // Loading skeleton on first load only — subsequent refetches keep the
  // last good data visible (no flicker).
  if (isLoading && !report) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Cluster Health</CardTitle>
          <CardDescription>Loading per-member health…</CardDescription>
        </CardHeader>
      </Card>
    )
  }

  // Hard error talking to the API endpoint itself (auth, 5xx, network).
  // Distinct from monitor_error which is the API succeeding but the
  // pg_auto_failover monitor being unreachable.
  if (error) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Cluster Health</CardTitle>
        </CardHeader>
        <CardContent>
          <Alert variant="destructive">
            <AlertDescription>
              Failed to load cluster health: {error.message}
            </AlertDescription>
          </Alert>
        </CardContent>
      </Card>
    )
  }

  if (!report) return null

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between gap-3">
          <div>
            <CardTitle>Cluster Health</CardTitle>
            <CardDescription>
              Live state from pg_auto_failover monitor — polls every{' '}
              {refetchInterval ? `${refetchInterval / 1000}s` : 'manual'}
            </CardDescription>
          </div>
          <div className="flex items-center gap-2">
            <div className="text-right text-xs text-muted-foreground">
              <div>
                Updated{' '}
                <TimeAgo
                  date={
                    dataUpdatedAt
                      ? new Date(dataUpdatedAt).toISOString()
                      : report.checked_at
                  }
                />
              </div>
              <div>monitor RTT {report.monitor_response_ms} ms</div>
            </div>
            <Button
              variant="outline"
              size="sm"
              onClick={() => refetch()}
              disabled={isFetching}
              title="Force a fresh probe of the monitor"
            >
              {isFetching ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <RefreshCw className="h-3.5 w-3.5" />
              )}
              <span className="ml-1.5">Refresh</span>
            </Button>
          </div>
        </div>
      </CardHeader>
      <CardContent>
        {report.monitor_error ? (
          <Alert variant="destructive">
            <AlertDescription>
              Monitor unreachable: {report.monitor_error}
            </AlertDescription>
          </Alert>
        ) : report.members.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            Monitor reports zero data nodes registered yet.
          </p>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-border text-left text-xs uppercase tracking-wide text-muted-foreground">
                  <th className="py-2 pr-3 font-medium">Health</th>
                  <th className="py-2 pr-3 font-medium">Node</th>
                  <th className="py-2 pr-3 font-medium">State</th>
                  <th className="py-2 pr-3 font-medium">Sync</th>
                  <th className="py-2 pr-3 font-medium text-right">Lag</th>
                  <th className="py-2 pr-3 font-medium text-right">Quorum</th>
                  <th className="py-2 pr-3 font-medium text-right">Priority</th>
                </tr>
              </thead>
              <tbody>
                {report.members.map((m) => (
                  <MemberRow key={m.nodename} member={m} />
                ))}
              </tbody>
            </table>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

function MemberRow({ member }: { member: ClusterMemberHealth }) {
  const bucket = bucketMemberHealth(member)
  const transitioning = isTransitioning(member)
  const dotTitle =
    bucket === 'unreachable'
      ? `Unreachable — last report ${formatLastSeen(member.seconds_since_report)}`
      : DOT_LABEL[bucket]
  return (
    <tr className="border-b border-border last:border-b-0">
      <td className="py-2.5 pr-3">
        <div className="flex items-center gap-2">
          <span
            className={`h-2.5 w-2.5 rounded-full ${DOT_CLASS[bucket]}`}
            aria-label={dotTitle}
            title={dotTitle}
          />
          <span className="sr-only">{dotTitle}</span>
        </div>
      </td>
      <td className="py-2.5 pr-3">
        <div className="flex flex-col">
          <span className="font-mono text-sm">{member.nodename}</span>
          <span className="text-xs text-muted-foreground">
            {member.nodehost}:{member.nodeport}
            {bucket === 'unreachable' && (
              <>
                {' · '}
                <span className="text-rose-400">
                  no report {formatLastSeen(member.seconds_since_report)}
                </span>
              </>
            )}
          </span>
        </div>
      </td>
      <td className="py-2.5 pr-3">
        <div className="flex items-center gap-1.5">
          {/* When stale-but-claiming-primary, strike through the badge so the
              user can see the schema's last self-report without trusting it. */}
          <Badge
            variant={reportedStateBadgeVariant(member.reported_state)}
            className={`capitalize ${
              bucket === 'unreachable' ? 'line-through opacity-60' : ''
            }`}
          >
            {member.reported_state.replace(/_/g, ' ')}
          </Badge>
          {transitioning && (
            <>
              <span className="text-muted-foreground" aria-label="transitioning to">
                →
              </span>
              <Badge
                variant={reportedStateBadgeVariant(member.goal_state)}
                className="capitalize"
                title={`Monitor wants this node to become "${member.goal_state}"`}
              >
                {member.goal_state.replace(/_/g, ' ')}
              </Badge>
            </>
          )}
        </div>
      </td>
      <td className="py-2.5 pr-3">
        {member.sync_state ? (
          <Badge variant={syncStateBadgeVariant(member.sync_state)}>
            {member.sync_state}
          </Badge>
        ) : (
          <span className="text-xs text-muted-foreground">—</span>
        )}
      </td>
      <td className="py-2.5 pr-3 text-right tabular-nums">
        {formatLag(member.replay_lag_ms)}
      </td>
      <td className="py-2.5 pr-3 text-right">
        {/*
          Quorum membership is a node *setting*, not a live signal.
          A stopped node still has `replicationquorum = true` in the
          monitor row — but it can't actually contribute to a quorum
          while it's unreachable. Show that as a hollow circle so the
          dot doesn't misrepresent live capacity.
        */}
        {bucket === 'unreachable' ? (
          <span
            className="text-muted-foreground"
            title="Node is unreachable; quorum-membership setting is irrelevant until it reports back"
          >
            ○
          </span>
        ) : member.replication_quorum ? (
          <span className="text-emerald-500" title="Member of the synchronous quorum">
            ●
          </span>
        ) : (
          <span className="text-muted-foreground" title="Not part of the quorum">
            ○
          </span>
        )}
      </td>
      <td className="py-2.5 pr-3 text-right tabular-nums text-muted-foreground">
        {member.candidate_priority}
      </td>
    </tr>
  )
}
