// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useSearchParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { Activity } from 'lucide-react'
import { Button, Callout, Ledger, PageState } from '@temps-sdk/ds'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@temps-sdk/ui'
import type { ClientOutcome, ProjectResponse } from '@/api/client'
import { listClientOutcomesOptions } from '@/api/client/@tanstack/react-query.gen'
import { readFailureServerDetail } from '@/lib/read-failure'

/** Collection screen: approximate SDK discards, with units kept separate. */
export default function ClientOutcomes({
  project,
}: {
  project: ProjectResponse
}) {
  const [params, setParams] = useSearchParams()
  const rawPage = Number(params.get('page') ?? 1)
  const page = Number.isSafeInteger(rawPage) && rawPage > 0 ? rawPage : 1
  const rawDays = Number(params.get('days') ?? 7)
  const days = [1, 7, 31].includes(rawDays) ? rawDays : 7
  const query = useQuery(
    listClientOutcomesOptions({
      path: { project_id: project.id },
      query: { days, page, page_size: 20 },
    })
  )
  const update = (key: string, value: string) =>
    setParams((previous) => {
      const next = new URLSearchParams(previous)
      next.set(key, value)
      if (key !== 'page') next.delete('page')
      return next
    })
  return (
    <Ledger<ClientOutcome>
      title="SDK ingestion health"
      description="See telemetry discarded inside your Sentry SDK before it reached Temps. Reports are approximate and retained for 31 days."
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
          <Select
            value={String(days)}
            onValueChange={(value) => update('days', value)}
          >
            <SelectTrigger
              className="w-full sm:w-44"
              aria-label="Outcome observation window"
            >
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
          <Callout tone="info" title="SDK-reported discards">
            Sampling and before-send filters can intentionally discard data.
            These totals describe SDK reports, not server rejections or stored
            events. Bytes, item counts and profile durations stay separate.
            Exact retries with the same envelope event ID, item position and
            report bytes count once for 24 hours. Reports without an event ID
            count independently, even when their content matches. Lost reports
            can still affect totals; future categories appear as “other” with an
            unknown unit.
          </Callout>
          {query.isError && (
            <Callout tone="error" title="Could not load SDK outcomes">
              {readFailureServerDetail(query.error) ??
                'Retry or select a shorter observation window.'}
            </Callout>
          )}
        </div>
      }
      rows={query.data?.items ?? []}
      rowKey={(row) => String(row.id)}
      isLoading={query.isPending}
      columns={[
        {
          key: 'bucket',
          header: 'Received hour',
          render: (row) => new Date(row.bucket).toLocaleString(),
        },
        {
          key: 'reason',
          header: 'Discard reason',
          render: (row) => (
            <span className="font-mono text-xs">{row.reason}</span>
          ),
        },
        { key: 'category', header: 'Category', render: (row) => row.category },
        {
          key: 'quantity',
          header: 'Reported quantity',
          render: (row) => (
            <span className="tabular-nums">
              {BigInt(row.quantity).toLocaleString()} {row.unit}
            </span>
          ),
        },
        {
          key: 'environment',
          header: 'Environment',
          render: (row) =>
            row.environment_id == null
              ? 'Project-wide'
              : `#${row.environment_id}`,
        },
        {
          key: 'deployment',
          header: 'Deployment',
          render: (row) =>
            row.deployment_id == null ? '—' : `#${row.deployment_id}`,
        },
      ]}
      empty={
        query.isError ? (
          <PageState
            variant="failed"
            icon={Activity}
            size="compact"
            title="Could not load SDK outcomes"
            description="Retry the query or reduce its observation window."
          />
        ) : (
          <PageState
            variant="not-set-up"
            icon={Activity}
            size="compact"
            title="No SDK discard reports"
            requirement="No SDK has reported discards during this window. This can also mean every item was sent successfully."
            example="Keep sendClientReports enabled in your Sentry SDK to see drops caused by sampling, filtering or a full transport queue."
            settingsHref={`/projects/${project.slug}/errors/setup`}
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
