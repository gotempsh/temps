// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { GitCommitHorizontal } from 'lucide-react'
import { useSearchParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { Button, Callout, Ledger, PageState, RecordLink } from '@temps-sdk/ds'
import {
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@temps-sdk/ui'
import type { ProjectResponse, TelemetryRelease } from '@/api/client'
import { listTelemetryReleasesOptions } from '@/api/client/@tanstack/react-query.gen'
import { readFailureServerDetail } from '@/lib/read-failure'

/** Collection screen: native release identity and retained signal correlation. */
export default function TelemetryReleases({
  project,
}: {
  project: ProjectResponse
}) {
  const [params, setParams] = useSearchParams()
  const [search, setSearch] = useState(params.get('release') ?? '')
  const rawPage = Number(params.get('page') ?? 1)
  const page = Number.isSafeInteger(rawPage) && rawPage > 0 ? rawPage : 1
  const rawDays = Number(params.get('days') ?? 7)
  const days = [1, 7, 31].includes(rawDays) ? rawDays : 7
  const query = useQuery(
    listTelemetryReleasesOptions({
      path: { project_id: project.id },
      query: {
        release: params.get('release') || undefined,
        days,
        page,
        page_size: 20,
      },
    })
  )
  const base = `/projects/${project.slug}`
  const update = (key: string, value: string) =>
    setParams((previous) => {
      const next = new URLSearchParams(previous)
      if (value) next.set(key, value)
      else next.delete(key)
      if (key !== 'page') next.delete('page')
      return next
    })
  const countLink = (count: number, href?: string) =>
    href && count ? (
      <RecordLink to={href}>{count.toLocaleString()}</RecordLink>
    ) : (
      count.toLocaleString()
    )
  const date = (stamp: string) => new Date(stamp).toLocaleString()
  return (
    <Ledger<TelemetryRelease>
      title="Releases"
      description="Correlate SDK release versions across retained native errors, traces, logs, metrics, feedback and cron check-ins. Counts cover the selected observation window."
      actions={
        <Button
          variant="outline"
          busy={query.isFetching}
          busyLabel="Refreshing…"
          onClick={() => void query.refetch()}
        >
          Refresh
        </Button>
      }
      toolbar={
        <div className="space-y-3">
          <form
            className="flex flex-wrap gap-2"
            aria-label="Filter releases"
            onSubmit={(event) => {
              event.preventDefault()
              update('release', search)
            }}
          >
            <Input
              className="min-w-0 flex-1 sm:max-w-sm"
              aria-label="Exact release version"
              placeholder="Exact release version…"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
            />
            <Button type="submit" variant="outline">
              Filter
            </Button>
            {params.has('release') && (
              <Button
                type="button"
                variant="ghost"
                onClick={() => {
                  setSearch('')
                  update('release', '')
                }}
              >
                Clear
              </Button>
            )}
            <Select
              value={String(days)}
              onValueChange={(value) => update('days', value)}
            >
              <SelectTrigger className="w-40" aria-label="Observation window">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {[1, 7, 31].map((value) => (
                  <SelectItem key={value} value={String(value)}>
                    Last {value} {value === 1 ? 'day' : 'days'}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </form>
          {query.isError && (
            <Callout tone="error" title="Could not load releases">
              {readFailureServerDetail(query.error) ??
                'Retry the query or select a shorter observation window.'}
            </Callout>
          )}
          <p className="text-xs text-muted-foreground">
            Available links open an example native record or series. First and
            last seen describe retained SDK observations, rather than deployment
            or finalization dates. Browser replay links are available from
            associated feedback and errors.
          </p>
        </div>
      }
      rows={query.data?.items ?? []}
      rowKey={(row) => row.release}
      isLoading={query.isPending}
      columns={[
        {
          key: 'release',
          header: 'Release',
          render: (row) => (
            <span className="break-all font-mono text-xs">{row.release}</span>
          ),
        },
        {
          key: 'errors',
          header: 'Errors',
          render: (row) =>
            countLink(
              row.errors,
              row.example_error_group_id
                ? `${base}/errors/${row.example_error_group_id}`
                : undefined
            ),
        },
        {
          key: 'spans',
          header: 'Spans',
          render: (row) =>
            countLink(
              row.spans,
              row.example_trace_id
                ? `${base}/traces/${row.example_trace_id}`
                : undefined
            ),
        },
        {
          key: 'logs',
          header: 'Logs',
          render: (row) =>
            countLink(
              row.logs,
              row.example_log_trace_id
                ? `${base}/telemetry-logs?trace=${row.example_log_trace_id}`
                : undefined
            ),
        },
        {
          key: 'metrics',
          header: 'Metric points',
          render: (row) =>
            countLink(
              row.metric_points,
              row.example_metric_name
                ? `${base}/metrics?metric=${encodeURIComponent(row.example_metric_name)}`
                : undefined
            ),
        },
        {
          key: 'feedback',
          header: 'Feedback',
          render: (row) =>
            countLink(
              row.feedback,
              row.example_feedback_id
                ? `${base}/errors/feedback/${row.example_feedback_id}`
                : undefined
            ),
        },
        {
          key: 'check_ins',
          header: 'Check-ins',
          render: (row) =>
            countLink(
              row.check_ins,
              row.example_monitor_id
                ? `${base}/check-ins/${row.example_monitor_id}`
                : undefined
            ),
        },
        {
          key: 'first_seen',
          header: 'First seen',
          render: (row) => date(row.first_seen),
        },
        {
          key: 'last_seen',
          header: 'Last seen',
          render: (row) => date(row.last_seen),
        },
      ]}
      empty={
        query.isError ? (
          <PageState
            variant="failed"
            icon={GitCommitHorizontal}
            size="compact"
            title="Could not load releases"
            description="Retry the query or reduce its observation window."
          />
        ) : (
          <PageState
            variant="not-set-up"
            icon={GitCommitHorizontal}
            size="compact"
            title="No observed releases"
            requirement="No retained signals match this release and observation window."
            example="Set release: 'my-app@1.0.0' in your Sentry SDK. Errors, traces, logs, metrics, feedback and cron check-ins will share that version here."
            settingsHref={`${base}/errors/setup`}
            settingsLabel="Connect SDK"
          />
        )
      }
      pagination={
        query.data
          ? {
              page,
              pageSize: 20,
              total: query.data.total,
              totalPages: Math.max(1, Math.ceil(query.data.total / 20)),
              onPageChange: (value) => update('page', String(value)),
            }
          : undefined
      }
    />
  )
}
