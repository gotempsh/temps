// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ProjectResponse,
  ReleaseHealth,
  ReleaseHealthPage,
  ReleaseHealthSeries,
} from '@/api/client'
import {
  getReleaseHealthSeriesOptions,
  listReleaseHealthOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ThresholdLineChart } from '@/components/charts/threshold-line-chart'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import { Skeleton } from '@/components/ui/skeleton'
import { readFailureServerDetail } from '@/lib/read-failure'
import {
  crashFreeAlertHref,
  crashFreeChartRows,
  DEFAULT_CRASH_FREE_THRESHOLD,
  formatRate,
  healthDaysFromParam,
  rateTone,
  releaseLabel,
  RELEASE_HEALTH_DAYS,
  type RateTone,
} from '@/lib/release-health'
import { hourTick } from '@/lib/operation-trend'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { BellPlus, HeartPulse, LineChart } from 'lucide-react'
import { useState } from 'react'
import { Link, useSearchParams } from 'react-router'

const ALL_ENVIRONMENTS = '__all__'

const TONE_CLASS: Record<RateTone, string> = {
  healthy: 'text-emerald-700 dark:text-emerald-400',
  warning: 'text-amber-700 dark:text-amber-400',
  critical: 'text-destructive',
  unknown: 'text-muted-foreground',
}

function Rate({ rate }: { rate: number | null | undefined }) {
  return (
    <span
      className={cn('tabular-nums font-medium', TONE_CLASS[rateTone(rate)])}
    >
      {formatRate(rate)}
    </span>
  )
}

/**
 * Crash-free sessions and users, adoption and the deployment of each
 * release, from the sessions Sentry SDKs send (ADR 111, D3).
 */
export function ReleaseHealthSection({
  project,
}: {
  project: ProjectResponse
}) {
  const [params, setParams] = useSearchParams()
  const days = healthDaysFromParam(params.get('health_days'))
  const environment = params.get('health_env') ?? ''
  const [open, setOpen] = useState<string | null>(null)
  const query = useQuery(
    listReleaseHealthOptions({
      path: { project_id: project.id },
      query: {
        days,
        per_page: 20,
        ...(environment ? { environment } : {}),
      },
    })
  )
  const update = (key: string, value: string) =>
    setParams((previous) => {
      const next = new URLSearchParams(previous)
      if (value) next.set(key, value)
      else next.delete(key)
      return next
    })

  return (
    <Card>
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div className="space-y-1">
          <CardTitle className="flex items-center gap-2 text-base">
            <HeartPulse className="size-4" aria-hidden />
            Release health
          </CardTitle>
          <CardDescription>
            How often each release crashes, for how many users, and how much of
            your traffic already runs it. Counted from the sessions your Sentry
            SDK sends; distinct users are an estimate.
          </CardDescription>
        </div>
        <Button asChild variant="outline" size="sm" className="shrink-0">
          <Link to={crashFreeAlertHref(project.slug)}>
            <BellPlus className="size-4" aria-hidden />
            Alert on a crash-free drop
          </Link>
        </Button>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="flex flex-wrap gap-2">
          <Select
            value={environment || ALL_ENVIRONMENTS}
            onValueChange={(value) =>
              update('health_env', value === ALL_ENVIRONMENTS ? '' : value)
            }
          >
            <SelectTrigger className="w-48" aria-label="SDK environment">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ALL_ENVIRONMENTS}>All environments</SelectItem>
              {(query.data?.environments ?? [])
                .filter((name) => name !== '')
                .map((name) => (
                  <SelectItem key={name} value={name}>
                    {name}
                  </SelectItem>
                ))}
            </SelectContent>
          </Select>
          <Select
            value={String(days)}
            onValueChange={(value) => update('health_days', value)}
          >
            <SelectTrigger className="w-40" aria-label="Release health window">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {RELEASE_HEALTH_DAYS.map((value) => (
                <SelectItem key={value} value={String(value)}>
                  Last {value} {value === 1 ? 'day' : 'days'}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        {query.isPending ? (
          <Skeleton className="h-32 w-full" />
        ) : query.isError ? (
          <Alert variant="destructive">
            <AlertDescription>
              {readFailureServerDetail(query.error) ??
                'Release health could not be loaded. Retry, or pick a shorter window.'}
            </AlertDescription>
          </Alert>
        ) : (
          <ReleaseHealthContent
            data={query.data}
            projectSlug={project.slug}
            onOpen={setOpen}
          />
        )}
      </CardContent>
      <ReleaseHealthSheet
        projectId={project.id}
        release={open}
        environment={environment || null}
        days={days}
        onClose={() => setOpen(null)}
      />
    </Card>
  )
}

/** The onboarding state or the table of a loaded release health page. */
export function ReleaseHealthContent({
  data,
  projectSlug,
  onOpen,
}: {
  data: ReleaseHealthPage
  projectSlug: string
  onOpen: (release: string) => void
}) {
  if (!data.tracking.configured) {
    return (
      <Alert>
        <AlertDescription className="space-y-2">
          <p>{data.tracking.reason}</p>
          <p className="text-muted-foreground">
            For example, in the browser:{' '}
            <code className="text-xs">
              {"Sentry.init({ dsn, release: 'my-app@1.0.0' })"}
            </code>{' '}
            sends a session on every page load, and a crash marks it crashed.
          </p>
          <Link
            className="font-medium underline"
            to={`/projects/${projectSlug}/${data.tracking.setup_path ?? 'errors/setup'}`}
          >
            Connect an SDK
          </Link>
        </AlertDescription>
      </Alert>
    )
  }
  if (data.items.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        No session started in this window
        {data.environments.length > 0 ? ' for this environment' : ''}.
      </p>
    )
  }
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-sm">
        <thead className="text-left text-xs text-muted-foreground">
          <tr>
            <th className="py-2 pr-3 font-medium">Release</th>
            <th className="py-2 pr-3 font-medium">Crash-free sessions</th>
            <th className="py-2 pr-3 font-medium">Crash-free users</th>
            <th className="py-2 pr-3 font-medium">Adoption (24h)</th>
            <th className="py-2 pr-3 font-medium">Sessions</th>
            <th className="py-2 pr-3 font-medium">Users</th>
            <th className="py-2 font-medium">
              <span className="sr-only">Trend</span>
            </th>
          </tr>
        </thead>
        <tbody className="divide-y">
          {data.items.map((row) => (
            <ReleaseHealthRow
              key={row.release}
              row={row}
              projectSlug={projectSlug}
              onOpen={onOpen}
            />
          ))}
        </tbody>
      </table>
      {data.total > data.items.length && (
        <p className="pt-2 text-xs text-muted-foreground">
          The {data.items.length} most recently active of {data.total} releases.
        </p>
      )}
    </div>
  )
}

function ReleaseHealthRow({
  row,
  projectSlug,
  onOpen,
}: {
  row: ReleaseHealth
  projectSlug: string
  onOpen: (release: string) => void
}) {
  const deployment = row.deployment
  return (
    <tr className="align-top">
      <td className="py-2 pr-3">
        <span className="block break-all font-mono text-xs" title={row.release}>
          {releaseLabel(row.release)}
        </span>
        {deployment ? (
          <Link
            className="text-xs text-muted-foreground hover:underline"
            to={`/projects/${projectSlug}/deployments/${deployment.deployment_id}`}
            title={deployment.commit_message ?? undefined}
          >
            Deployed
            {deployment.environment_name
              ? ` to ${deployment.environment_name}`
              : ''}
            {deployment.ready_at
              ? ` · ${new Date(deployment.ready_at).toLocaleString()}`
              : ''}
          </Link>
        ) : (
          <span className="text-xs text-muted-foreground">
            Not deployed by Temps
          </span>
        )}
      </td>
      <td className="py-2 pr-3">
        <Rate rate={row.crash_free_sessions} />
        {row.crashed_sessions > 0 && (
          <span className="block text-xs text-muted-foreground">
            {row.crashed_sessions.toLocaleString()} crashed
          </span>
        )}
      </td>
      <td className="py-2 pr-3">
        <Rate rate={row.crash_free_users} />
      </td>
      <td className="py-2 pr-3 tabular-nums">{formatRate(row.adoption)}</td>
      <td className="py-2 pr-3 tabular-nums">
        {row.sessions.toLocaleString()}
        {row.errored_sessions > 0 && (
          <span className="block text-xs text-muted-foreground">
            {row.errored_sessions.toLocaleString()} with errors
          </span>
        )}
      </td>
      <td className="py-2 pr-3 tabular-nums">~{row.users.toLocaleString()}</td>
      <td className="py-2 text-right">
        <Button
          variant="ghost"
          size="icon"
          aria-label={`Crash-free trend of ${row.release}`}
          onClick={() => onOpen(row.release)}
        >
          <LineChart className="size-4" aria-hidden />
        </Button>
      </td>
    </tr>
  )
}

function ReleaseHealthSheet({
  projectId,
  release,
  environment,
  days,
  onClose,
}: {
  projectId: number
  release: string | null
  environment: string | null
  days: number
  onClose: () => void
}) {
  const query = useQuery({
    ...getReleaseHealthSeriesOptions({
      path: { project_id: projectId },
      query: {
        release: release ?? '',
        days,
        ...(environment ? { environment } : {}),
      },
    }),
    enabled: release !== null,
  })
  return (
    <Sheet open={release !== null} onOpenChange={(next) => !next && onClose()}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-xl">
        <SheetHeader>
          <SheetTitle className="break-all">
            {release ? releaseLabel(release) : ''}
          </SheetTitle>
          <SheetDescription>
            Crash-free sessions per hour
            {environment ? ` in ${environment}` : ''}, by the hour each session
            started
          </SheetDescription>
        </SheetHeader>
        <div className="space-y-4 px-4 pb-6">
          {query.isPending ? (
            <Skeleton className="h-64 w-full" />
          ) : query.isError ? (
            <Alert variant="destructive">
              <AlertDescription>
                {readFailureServerDetail(query.error) ??
                  'The release trend could not be loaded.'}
              </AlertDescription>
            </Alert>
          ) : (
            <ReleaseHealthSeriesContent series={query.data} />
          )}
        </div>
      </SheetContent>
    </Sheet>
  )
}

/** The chart and totals of a loaded release series. */
export function ReleaseHealthSeriesContent({
  series,
}: {
  series: ReleaseHealthSeries
}) {
  if (series.points.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        No session of this release started in the window.
      </p>
    )
  }
  const rows = crashFreeChartRows(series.points)
  const sessions = series.points.reduce((sum, p) => sum + p.sessions, 0)
  const crashed = series.points.reduce((sum, p) => sum + p.crashed_sessions, 0)
  const abnormal = series.points.reduce(
    (sum, p) => sum + p.abnormal_sessions,
    0
  )
  return (
    <>
      <div className="flex flex-wrap gap-2">
        <Badge variant="outline">{sessions.toLocaleString()} sessions</Badge>
        <Badge variant={crashed > 0 ? 'destructive' : 'outline'}>
          {crashed.toLocaleString()} crashed
        </Badge>
        {abnormal > 0 && (
          <Badge variant="outline">{abnormal.toLocaleString()} abnormal</Badge>
        )}
      </div>
      <ThresholdLineChart
        data={rows}
        xKey="hour"
        series={{
          dataKey: 'crashFree',
          label: 'Crash-free sessions',
          tone: 'primary',
        }}
        thresholds={[
          {
            value: DEFAULT_CRASH_FREE_THRESHOLD * 100,
            tone: 'warn',
            label: `${DEFAULT_CRASH_FREE_THRESHOLD * 100}%`,
          },
        ]}
        height={260}
        allowDecimals
        xTickFormatter={hourTick}
        yTickFormatter={(value) => `${value}%`}
        tooltipValueFormatter={(value) => `${value}%`}
      />
      <p className="text-xs text-muted-foreground">
        A session counts in the hour it started. An hour with few sessions
        swings widely: one crash in ten sessions is 90%.
      </p>
    </>
  )
}
