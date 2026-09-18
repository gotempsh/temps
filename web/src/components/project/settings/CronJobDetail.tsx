// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getCronByIdOptions,
  getCronExecutionsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { CronExecutionInfo, ProjectResponse } from '@/api/client'
import { useQuery } from '@tanstack/react-query'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  Skeleton,
} from '@temps-sdk/ui'
import { Clock, ArrowLeft, AlertCircle } from 'lucide-react'
import {
  Button,
  Callout,
  DataTable,
  PageState,
  Status,
  fmtDateTime,
  fmtDuration,
  type DataTableColumn,
} from '@temps-sdk/ds'
import { useParams } from 'react-router'
import { useGoBack } from '@/hooks/useGoBack'

const executionColumns: DataTableColumn<CronExecutionInfo>[] = [
  {
    key: 'time',
    header: 'Time',
    render: (execution) => fmtDateTime(execution.executed_at),
  },
  {
    key: 'status',
    header: 'Status',
    render: (execution) => {
      const succeeded =
        execution.status_code >= 200 && execution.status_code < 300
      return (
        <Status
          tone={succeeded ? 'ok' : 'error'}
          label={succeeded ? 'Success' : 'Failed'}
        />
      )
    },
  },
  {
    key: 'duration',
    header: 'Response Time',
    render: (execution) => fmtDuration(execution.response_time_ms),
  },
  {
    key: 'details',
    header: 'Details',
    render: (execution) => (
      <div className="space-y-1">
        <div>Status: {execution.status_code}</div>
        {execution.error_message && (
          <div className="text-destructive whitespace-normal break-words">
            {execution.error_message}
          </div>
        )}
      </div>
    ),
  },
]

interface CronJobDetailProps {
  project: ProjectResponse
}

export function CronJobDetail({ project }: CronJobDetailProps) {
  const goBack = useGoBack(`/projects/${project.slug}/settings/cron-jobs`)
  const { environmentId, cronId } = useParams<{
    environmentId: string
    cronId: string
  }>()
  const cronQuery = useQuery({
    ...getCronByIdOptions({
      path: {
        project_id: project.id,
        env_id: Number(environmentId),
        cron_id: Number(cronId),
      },
    }),
  })

  const executionsQuery = useQuery({
    ...getCronExecutionsOptions({
      path: {
        project_id: project.id,
        env_id: Number(environmentId),
        cron_id: Number(cronId),
      },
      query: {
        page: 1,
        per_page: 10,
      },
    }),
  })

  const cronJob = cronQuery.data
  const executions = executionsQuery.data

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-4">
        <Button
          variant="ghost"
          size="icon"
          aria-label="Back to cron jobs"
          onClick={() => goBack()}
        >
          <ArrowLeft className="size-4" />
        </Button>
        <div>
          <h2 className="text-lg font-medium">Cron Job Details</h2>
          <p className="text-sm text-muted-foreground">
            View cron job configuration and execution history
          </p>
        </div>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Configuration</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          {cronQuery.isError && cronJob && (
            <Callout tone="error" title="Couldn't refresh cron job">
              Showing the last loaded configuration.{' '}
              <Button
                variant="outline"
                size="sm"
                onClick={() => void cronQuery.refetch()}
                busy={cronQuery.isFetching}
                busyLabel="Retrying…"
              >
                Retry configuration
              </Button>
            </Callout>
          )}
          {cronQuery.isLoading ? (
            <div
              className="grid gap-4 md:grid-cols-2"
              aria-label="Loading cron job configuration"
            >
              {[0, 1, 2, 3].map((index) => (
                <div key={index} className="space-y-2">
                  <Skeleton className="h-4 w-20" />
                  <Skeleton className="h-5 w-40" />
                </div>
              ))}
            </div>
          ) : !cronJob ? (
            <PageState
              variant="failed"
              size="compact"
              icon={AlertCircle}
              title="Couldn't load cron job"
              description="The configuration could not be loaded. Retry to check this cron job's schedule."
              action={
                <Button
                  onClick={() => void cronQuery.refetch()}
                  busy={cronQuery.isFetching}
                  busyLabel="Retrying…"
                >
                  Retry configuration
                </Button>
              }
            />
          ) : (
            <dl className="grid gap-4 md:grid-cols-2">
              <div>
                <dt className="text-sm font-medium">Path</dt>
                <dd>
                  <code className="text-sm break-all">{cronJob.path}</code>
                </dd>
              </div>
              <div>
                <dt className="text-sm font-medium">Schedule</dt>
                <dd>
                  <code className="text-sm">{cronJob.schedule}</code>
                </dd>
              </div>
              <div>
                <dt className="text-sm font-medium">Next Run</dt>
                <dd className="text-sm text-muted-foreground">
                  {cronJob.next_run
                    ? fmtDateTime(cronJob.next_run)
                    : 'Not scheduled'}
                </dd>
              </div>
              <div>
                <dt className="text-sm font-medium">Created</dt>
                <dd className="text-sm text-muted-foreground">
                  {fmtDateTime(cronJob.created_at)}
                </dd>
              </div>
            </dl>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Recent Executions</CardTitle>
          <CardDescription>Last 10 executions of this cron job</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          {executionsQuery.isError && executions && executions.length > 0 && (
            <Callout tone="error" title="Couldn't refresh executions">
              Showing the last loaded executions.{' '}
              <Button
                variant="outline"
                size="sm"
                onClick={() => void executionsQuery.refetch()}
                busy={executionsQuery.isFetching}
                busyLabel="Retrying…"
              >
                Retry executions
              </Button>
            </Callout>
          )}
          {executionsQuery.isError && !executions?.length ? (
            <PageState
              variant="failed"
              size="compact"
              icon={AlertCircle}
              title="Couldn't load executions"
              description="Execution history is unavailable. Retry to see recent runs."
              action={
                <Button
                  onClick={() => void executionsQuery.refetch()}
                  busy={executionsQuery.isFetching}
                  busyLabel="Retrying…"
                >
                  Retry executions
                </Button>
              }
            />
          ) : !executionsQuery.isLoading && !executions?.length ? (
            <PageState
              variant="empty"
              size="compact"
              icon={Clock}
              title="No executions yet"
              description="Execution history will appear after this cron job runs."
            />
          ) : (
            <DataTable
              aria-label="Recent cron job executions"
              columns={executionColumns}
              rows={executions ?? []}
              rowKey={(execution) => execution.id}
              isLoading={executionsQuery.isLoading}
            />
          )}
        </CardContent>
      </Card>
    </div>
  )
}
