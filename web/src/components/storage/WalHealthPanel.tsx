// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Postgres WAL health surface on the service detail page.
 *
 * Renders nothing when the probe has no warnings — the absence of an alert
 * is the success state. When warnings are present, shows a single Alert
 * with one row per warning and a remediation SQL snippet the operator can
 * copy/paste.
 */
import {
  formatBytes,
  getPostgresWalHealth,
  severityOf,
  type WalWarning,
} from '@/lib/wal-health'
import { getExternalServiceBackupCapabilityOptions } from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { getErrorMessage } from '@/utils/errorHandling'
import { useQuery } from '@tanstack/react-query'
import { AlertTriangle, ShieldAlert } from 'lucide-react'

interface Props {
  serviceId: number
  serviceType: string
  onUpgrade?: () => void
}

export function WalHealthPanel({ serviceId, serviceType, onUpgrade }: Props) {
  // Only Postgres services produce WAL health snapshots. Bail out early so
  // we don't spam the API with 404s for Redis / Mongo / S3 services.
  const enabled = serviceType === 'postgres'

  const { data } = useQuery({
    queryKey: ['wal-health', serviceId],
    queryFn: () => getPostgresWalHealth(serviceId),
    enabled,
    refetchInterval: 60_000,
    staleTime: 55_000,
  })
  const backupCapability = useQuery({
    ...getExternalServiceBackupCapabilityOptions({ path: { id: serviceId } }),
    enabled,
    refetchInterval: 60_000,
    staleTime: 55_000,
    retry: false,
  })

  const snapshot = data?.wal_health ?? null
  const incompatible = backupCapability.data?.cloud_backup_compatible === false
  if (
    (!snapshot || snapshot.warnings.length === 0) &&
    !incompatible &&
    !backupCapability.isError
  ) {
    return null
  }

  const hasCritical =
    snapshot?.warnings.some((w) => severityOf(w) === 'critical') ?? false

  return (
    <div className="space-y-3">
      {backupCapability.isError ? (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertDescription className="space-y-3">
            <div>
              <p className="font-medium">Backup compatibility unavailable</p>
              <p className="mt-1 text-xs text-muted-foreground">
                {getErrorMessage(
                  backupCapability.error,
                  'The server could not check whether this service supports Cloud backups.'
                )}
              </p>
            </div>
            <Button
              size="sm"
              variant="outline"
              onClick={() => void backupCapability.refetch()}
              disabled={backupCapability.isFetching}
            >
              Check again
            </Button>
          </AlertDescription>
        </Alert>
      ) : null}
      {incompatible ? (
        <Alert>
          <AlertTriangle className="h-4 w-4" />
          <AlertDescription className="space-y-3">
            <div>
              <p className="font-medium">Cloud backups need WAL-G</p>
              <p className="mt-1 text-xs text-muted-foreground">
                {backupCapability.data?.reason}
              </p>
              {backupCapability.data?.remediation ? (
                <p className="mt-1 text-xs text-muted-foreground">
                  {backupCapability.data.remediation}
                </p>
              ) : null}
            </div>
            {onUpgrade ? (
              <Button size="sm" onClick={onUpgrade}>
                Upgrade backup engine
              </Button>
            ) : null}
          </AlertDescription>
        </Alert>
      ) : null}
      {snapshot && snapshot.warnings.length > 0 ? (
        <Alert variant={hasCritical ? 'destructive' : 'default'}>
          {hasCritical ? (
            <ShieldAlert className="h-4 w-4" />
          ) : (
            <AlertTriangle className="h-4 w-4" />
          )}
          <AlertDescription className="space-y-3">
            <div className="flex items-baseline justify-between gap-3">
              <p className="font-medium">
                {hasCritical
                  ? 'WAL is filling disk'
                  : 'WAL health needs attention'}
              </p>
              <p className="text-xs text-muted-foreground">
                Probed <TimeAgo date={snapshot.probed_at} />
              </p>
            </div>

            <div className="flex flex-wrap gap-x-5 gap-y-1 text-xs text-muted-foreground">
              <span>
                <span className="font-medium text-foreground">
                  {formatBytes(snapshot.pg_wal_bytes)}
                </span>{' '}
                in pg_wal
              </span>
              <span>
                max_wal_size:{' '}
                <span className="font-medium text-foreground">
                  {formatBytes(snapshot.max_wal_size_bytes)}
                </span>
              </span>
              <span>
                archive_mode:{' '}
                <span className="font-medium text-foreground">
                  {snapshot.archive_mode}
                </span>
              </span>
            </div>

            <ul className="space-y-2">
              {snapshot.warnings.map((w, idx) => (
                <WarningRow key={`${w.kind}-${idx}`} warning={w} />
              ))}
            </ul>
          </AlertDescription>
        </Alert>
      ) : null}
    </div>
  )
}

function WarningRow({ warning }: { warning: WalWarning }) {
  const { title, body, fix } = describeWarning(warning)
  return (
    <li className="flex flex-col gap-1 border-t border-border/50 pt-2 first:border-t-0 first:pt-0">
      <p className="text-sm font-medium">{title}</p>
      {body ? <p className="text-xs opacity-80">{body}</p> : null}
      {fix ? (
        <div className="flex items-center gap-2">
          <code className="block flex-1 truncate rounded bg-background/60 px-2 py-1 font-mono text-[11px]">
            {fix}
          </code>
          <CopyButton value={fix} />
        </div>
      ) : null}
    </li>
  )
}

interface WarningView {
  title: string
  body?: string
  fix?: string
}

function describeWarning(w: WalWarning): WarningView {
  switch (w.kind) {
    case 'wal_bloat':
      return {
        title: `pg_wal is ${w.ratio.toFixed(1)}× max_wal_size`,
        body: `WAL has grown to ${formatBytes(w.pg_wal_bytes)} against a max_wal_size of ${formatBytes(w.max_wal_size_bytes)}. Usually caused by a stuck replication slot, a failing archive_command, or an open transaction.`,
      }
    case 'stale_slot':
      return {
        title: `Replication slot "${w.slot_name}" is retaining ${formatBytes(w.retained_bytes)} of WAL`,
        body: w.active
          ? 'Slot is active but its consumer is lagging.'
          : 'Slot is inactive — likely abandoned by a dead replica.',
        fix: `SELECT pg_drop_replication_slot('${w.slot_name}');`,
      }
    case 'archive_backlog':
      return {
        title: `${w.ready_count} WAL segments queued for archiving`,
        body: 'archive_command is failing or running slower than WAL generation. Check the container logs for the archiver process.',
      }
    case 'archive_mode_without_command':
      return {
        title: 'archive_mode is on, but archive_command is empty',
        body: 'WAL is being held forever waiting for a destination that never accepts it. Stop and start this service from the actions menu — Temps reconciles archive_mode on start and the container will come back up with archive_mode=off (or =on if you’ve since configured WAL-G).',
      }
    case 'wal_not_recycled':
      return {
        title: `Oldest WAL segment is ${Math.round(w.oldest_age_secs / 3600)}h old`,
        body: 'WAL recycling has stalled. Run CHECKPOINT and check pg_replication_slots / pg_stat_archiver for the underlying cause.',
        fix: 'CHECKPOINT;',
      }
  }
}
