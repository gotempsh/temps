// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Continuous WAL archiving on the PostgreSQL service page: when WAL last
 * reached the archive, the last failure and the segment it failed on, how
 * much WAL is waiting, a "Test archive target" action, and whether the
 * archiving alarms would reach anyone.
 */
import {
  getNotificationDeliveryCoverageOptions,
  testWalArchiveTargetMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ArchiveTargetTestResponse,
  NotificationDeliveryCoverageResponse,
  PostgresWalHealth,
} from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { problemDetail, problemSetupPath } from '@/lib/api-problem'
import { locationPath, withReturnTo } from '@/lib/safe-return-to'
import {
  archivingState,
  formatBytes,
  type ArchivingState,
} from '@/lib/wal-health'
import { Callout } from '@temps-sdk/ds'
import { useMutation, useQuery } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { Link, useLocation } from 'react-router'
import { toast } from 'sonner'

const linkClass = 'font-medium underline underline-offset-4'

const STATE_LABEL: Record<ArchivingState, string> = {
  archiving: 'Archiving',
  failing: 'Failing',
  waiting: 'No WAL archived yet',
  discarding: 'Not storing WAL: archive_command does nothing',
  no_command:
    'Not archiving: archive_command is empty, so pg_wal keeps growing',
}

const STATE_CLASS: Record<ArchivingState, string> = {
  archiving: 'text-emerald-700 dark:text-emerald-400',
  failing: 'text-destructive',
  waiting: 'text-muted-foreground',
  discarding: 'text-amber-700 dark:text-amber-400',
  no_command: 'text-amber-700 dark:text-amber-400',
}

interface Props {
  serviceId: number
  snapshot: PostgresWalHealth
}

/**
 * Keyed by service: the page stays mounted when only its `:id` changes, and
 * a check's result (or one still running) belongs to the service it ran on.
 */
export function WalArchivingStatus(props: Props) {
  return <WalArchivingStatusForService key={props.serviceId} {...props} />
}

function WalArchivingStatusForService({ serviceId, snapshot }: Props) {
  const location = useLocation()
  const coverage = useQuery({
    ...getNotificationDeliveryCoverageOptions({
      query: { severity: 'critical' },
    }),
    retry: false,
  })
  const test = useMutation({
    ...testWalArchiveTargetMutation(),
    onSuccess: (result) => {
      if (!result.ok) toast.error('WAL-G could not write to the archive target')
      else if (result.live_settings_drift)
        toast.warning(
          'The archive target works, but the running database uses different settings'
        )
      else toast.success('WAL-G can write to the archive target')
    },
    onError: (error) => {
      toast.error(problemDetail(error, 'Could not test the archive target.'))
    },
  })

  return (
    <WalArchivingStatusView
      snapshot={snapshot}
      coverage={coverage.data}
      returnTo={locationPath(location)}
      testing={test.isPending}
      result={test.data}
      testError={
        test.isError
          ? {
              message: problemDetail(
                test.error,
                'Could not test the archive target.'
              ),
              setupPath: problemSetupPath(test.error),
            }
          : undefined
      }
      onTest={() => test.mutate({ path: { id: serviceId } })}
    />
  )
}

function WaitingSegments({ snapshot }: { snapshot: PostgresWalHealth }) {
  if (snapshot.archive_status_unreadable)
    return <>Unknown: the database role may not list archive_status</>
  return (
    <>
      {snapshot.archive_backlog}{' '}
      {snapshot.archive_backlog === 1 ? 'segment' : 'segments'}
      {snapshot.archive_oldest_ready_age_secs ? (
        <>
          , oldest {Math.round(snapshot.archive_oldest_ready_age_secs / 60)} min
        </>
      ) : null}
    </>
  )
}

interface ViewProps {
  snapshot: PostgresWalHealth
  coverage?: NotificationDeliveryCoverageResponse
  returnTo: string
  testing: boolean
  result?: ArchiveTargetTestResponse
  testError?: { message: string; setupPath?: string }
  onTest: () => void
}

export function WalArchivingStatusView({
  snapshot,
  coverage,
  returnTo,
  testing,
  result,
  testError,
  onTest,
}: ViewProps) {
  const state = archivingState(snapshot)
  return (
    <section
      aria-label="WAL archiving"
      className="space-y-3 rounded-md border p-4"
    >
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <p className="text-sm font-medium">
          WAL archiving:{' '}
          <span className={STATE_CLASS[state]}>{STATE_LABEL[state]}</span>
        </p>
        <Button size="sm" variant="outline" onClick={onTest} disabled={testing}>
          {testing ? <Loader2 className="h-4 w-4 animate-spin" /> : null}
          Test archive target
        </Button>
      </div>

      <dl className="grid grid-cols-2 gap-x-5 gap-y-2 text-xs md:grid-cols-3 xl:grid-cols-5">
        <Fact label="Last archived">
          {snapshot.archiver_last_archived_at ? (
            <TimeAgo date={snapshot.archiver_last_archived_at} />
          ) : (
            'Never'
          )}
        </Fact>
        <Fact label="Last failure">
          {snapshot.archiver_last_failed_at ? (
            <TimeAgo date={snapshot.archiver_last_failed_at} />
          ) : (
            'None'
          )}
        </Fact>
        <Fact label="Failed segment">
          <span className="font-mono [overflow-wrap:anywhere]">
            {snapshot.archiver_last_failed_wal ?? '—'}
          </span>
        </Fact>
        <Fact label="Waiting to archive">
          <WaitingSegments snapshot={snapshot} />
        </Fact>
        <Fact label="pg_wal">
          {formatBytes(snapshot.pg_wal_bytes)} of{' '}
          {formatBytes(snapshot.max_wal_size_bytes)} max_wal_size
        </Fact>
      </dl>

      {result ? <TestResult result={result} /> : null}
      {testError ? (
        <Callout tone="error" title="The archive target could not be tested">
          {testError.message}
          {testError.setupPath ? (
            <>
              {' '}
              <Link to={testError.setupPath} className={linkClass}>
                Fix it here
              </Link>
              .
            </>
          ) : null}
        </Callout>
      ) : null}

      {coverage && !coverage.configured ? (
        <Callout
          tone="warning"
          title="Archiving alarms won't notify anyone yet"
        >
          {coverage.reason ?? 'No notification destination is configured'}. The
          &ldquo;WAL archiving failing&rdquo; and &ldquo;WAL backlog&rdquo;
          alarms still fire on this page.{' '}
          <Link
            to={withReturnTo(
              coverage.setup_path ?? '/settings/notifications/new',
              returnTo
            )}
            className={linkClass}
          >
            Add a notification provider
          </Link>{' '}
          to be told when WAL stops reaching the archive.
        </Callout>
      ) : null}
    </section>
  )
}

function Fact({
  label,
  children,
}: {
  label: string
  children: React.ReactNode
}) {
  return (
    <div className="min-w-0">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="font-medium">{children}</dd>
    </div>
  )
}

function TestResult({ result }: { result: ArchiveTargetTestResponse }) {
  return (
    <>
      <TestOutcome result={result} />
      {result.live_settings_drift ? (
        <Callout
          tone="warning"
          title="The running database archives with different settings"
        >
          <span className="block [overflow-wrap:anywhere]">
            {result.live_settings_drift}
          </span>
        </Callout>
      ) : null}
    </>
  )
}

function TestOutcome({ result }: { result: ArchiveTargetTestResponse }) {
  return (
    <Callout
      tone={result.ok ? 'success' : 'error'}
      title={
        result.ok
          ? 'WAL-G can write to the archive target'
          : 'WAL-G could not write to the archive target'
      }
    >
      <span className="block [overflow-wrap:anywhere]">
        {result.endpoint} (scheme {result.scheme}, path-style addressing{' '}
        {result.force_path_style ? 'on' : 'off'}, {result.path_style_reason}),
        S3 source {result.s3_source_name}.
      </span>
      <span className="mt-1 block font-mono text-xs [overflow-wrap:anywhere]">
        {result.message}
      </span>
      <span className="mt-1 block text-xs text-muted-foreground">
        Checked <TimeAgo date={result.checked_at} />
      </span>
    </Callout>
  )
}
