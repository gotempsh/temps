// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { DataTable, Detail, PageState, Status } from '@temps-sdk/ds'
import { Workflow } from 'lucide-react'
import { Link, useParams, useSearchParams } from 'react-router'
import {
  getWorkflowRunOptions,
  listWorkflowStepsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { RefreshButton } from '@/components/ui/refresh-button'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { usePageTitle } from '@/hooks/usePageTitle'
import { problemDetail } from '@/lib/api-problem'
import { workflowTone } from './status'

export function WorkflowRunDetail() {
  const params = useParams()
  const [search, setSearch] = useSearchParams()
  const page = Math.max(1, Number(search.get('page')) || 1)
  const path = {
    project_id: Number(params.projectId),
    environment_id: Number(params.environmentId),
    run_id: params.runId ?? '',
  }
  const runOptions = getWorkflowRunOptions({ path })
  const stepOptions = listWorkflowStepsOptions({
    path,
    query: { page, page_size: 20 },
  })
  const run = useQuery(runOptions)
  const steps = useQuery(stepOptions)
  usePageTitle('Workflow run')
  const refresh = (
    <RefreshButton queryKeys={[runOptions.queryKey, stepOptions.queryKey]} />
  )
  if (run.isError)
    return (
      <PageState
        variant="failed"
        size="compact"
        icon={Workflow}
        title="Could not load this run"
        description={problemDetail(run.error, 'Refresh to try again.')}
        action={refresh}
      />
    )
  return (
    <Detail
      title={run.data?.workflow ?? 'Workflow run'}
      actions={
        <div className="flex flex-wrap gap-2">
          <Button asChild variant="outline">
            <Link to={`/workflows/${path.project_id}/keys`}>
              Encryption keys
            </Link>
          </Button>
          <Button asChild variant="outline">
            <Link
              to={`/workflows/${path.project_id}/${path.environment_id}/grants`}
            >
              Access grants
            </Link>
          </Button>
          <Button asChild variant="outline">
            <Link to="/workflows">All workflows</Link>
          </Button>
          {refresh}
        </div>
      }
      verdict={
        run.data ? (
          <Status
            tone={workflowTone(run.data.status)}
            label={run.data.status}
          />
        ) : (
          <Skeleton className="h-6 w-24" />
        )
      }
      facts={[
        { label: 'Project', value: path.project_id },
        { label: 'Environment', value: path.environment_id },
        { label: 'Attempt', value: run.data?.attempt ?? '—' },
        { label: 'Steps', value: run.data?.step_count ?? '—' },
        {
          label: 'Started',
          value: run.data
            ? new Date(run.data.created_at).toLocaleString()
            : '—',
        },
      ]}
      main={
        <div className="space-y-4">
          <p className="text-sm text-muted-foreground [overflow-wrap:anywhere]">
            Run {path.run_id}
          </p>
          {run.data?.blocked_reason && (
            <p role="status" className="text-sm">
              {run.data.blocked_reason}
            </p>
          )}
          {run.data?.error_kind && (
            <p className="text-sm text-destructive">{run.data.error_kind}</p>
          )}
          {steps.isError ? (
            <p role="alert">
              {problemDetail(
                steps.error,
                'Could not load the journal. Refresh to retry.'
              )}
            </p>
          ) : (
            <DataTable
              aria-label="Workflow journal"
              rows={steps.data?.steps ?? []}
              rowKey={(step) => step.sequence}
              isLoading={steps.isPending}
              columns={[
                {
                  key: 'name',
                  header: 'Step',
                  render: (step) => (
                    <span className="[overflow-wrap:anywhere]">
                      {step.sequence + 1}. {step.name}
                    </span>
                  ),
                },
                {
                  key: 'kind',
                  header: 'Kind',
                  className: 'hidden @xl:table-cell',
                  render: (step) => step.kind,
                },
                {
                  key: 'status',
                  header: 'State',
                  render: (step) => (
                    <Status
                      tone={workflowTone(step.status)}
                      label={step.status}
                    />
                  ),
                },
                {
                  key: 'attempts',
                  header: 'Attempts',
                  className: 'hidden @xl:table-cell',
                  render: (step) => step.attempts,
                },
              ]}
              pagination={{
                page,
                pageSize: 20,
                total: steps.data?.total ?? 0,
                totalPages: Math.max(1, steps.data?.total_pages ?? 1),
                onPageChange: (value) => setSearch({ page: String(value) }),
              }}
            />
          )}
        </div>
      }
    />
  )
}
