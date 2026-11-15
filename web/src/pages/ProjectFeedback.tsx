// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import {
  Link,
  Route,
  Routes,
  useNavigate,
  useParams,
  useSearchParams,
} from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { MessageSquare } from 'lucide-react'
import {
  Button,
  Callout,
  Detail,
  Ledger,
  PageState,
  RecordLink,
  Status,
} from '@temps-sdk/ds'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@temps-sdk/ui'
import type { FeedbackView, ProjectResponse } from '@/api/client'
import { deleteProjectFeedback, updateProjectFeedback } from '@/api/client'
import {
  getProjectFeedbackOptions,
  getProjectFeedbackQueryKey,
  listProjectFeedbackOptions,
  listProjectFeedbackQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { RefreshButton } from '@/components/ui/refresh-button'
import { queryKeyPrefix } from '@/lib/refresh'
import { CodeBlock } from '@/components/ui/code-block'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { ScrollRegion } from '@/components/ui/scroll-region'
import { readFailureServerDetail } from '@/lib/read-failure'

const failure = (error: unknown) =>
  readFailureServerDetail(error) ??
  'Temps could not load feedback. Please retry.'
const base = (project: ProjectResponse) =>
  `/projects/${project.slug}/errors/feedback`
const stamp = (value: string) => new Date(value).toLocaleString()
const verdict = (status: string) => (
  <Status
    tone={status === 'resolved' ? 'ok' : status === 'open' ? 'warn' : 'idle'}
    label={status}
  />
)

function FeedbackList({ project }: { project: ProjectResponse }) {
  const [params, setParams] = useSearchParams()
  const rawPage = Number(params.get('page') ?? 1)
  const page = Number.isSafeInteger(rawPage) && rawPage > 0 ? rawPage : 1
  const status = params.get('status') ?? 'open'
  const query = useQuery(
    listProjectFeedbackOptions({
      path: { project_id: project.id },
      query: {
        page,
        page_size: 20,
        status: status === 'all' ? undefined : status,
      },
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
    <Ledger<FeedbackView>
      title="Feedback"
      description="User reports sent from your application, linked to errors and browser recordings."
      actions={
        <RefreshButton
          queryKeys={[
            queryKeyPrefix(listProjectFeedbackQueryKey, {
              path: { project_id: project.id },
            }),
          ]}
        />
      }
      toolbar={
        <div className="space-y-3">
          <Select
            value={status}
            onValueChange={(value) => update('status', value)}
          >
            <SelectTrigger className="w-44" aria-label="Feedback status">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {['open', 'resolved', 'ignored', 'all'].map((value) => (
                <SelectItem key={value} value={value}>
                  {value === 'all'
                    ? 'All feedback'
                    : value[0].toUpperCase() + value.slice(1)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          {query.isError && (
            <Callout tone="error" title="Could not load feedback">
              {failure(query.error)}
            </Callout>
          )}
        </div>
      }
      rows={query.data?.items ?? []}
      rowKey={(item) => item.id}
      isLoading={query.isPending}
      columns={[
        {
          key: 'message',
          header: 'Report',
          render: (item) => (
            <RecordLink to={`${base(project)}/${item.id}`}>
              <span className="line-clamp-2 break-words">{item.message}</span>
            </RecordLink>
          ),
        },
        {
          key: 'status',
          header: 'Status',
          render: (item) => verdict(item.status),
        },
        {
          key: 'name',
          header: 'From',
          render: (item) => item.name ?? item.email ?? 'Anonymous',
        },
        {
          key: 'release',
          header: 'Release',
          render: (item) => item.release ?? '—',
        },
        {
          key: 'received',
          header: 'Received',
          render: (item) => stamp(item.received_at),
        },
      ]}
      empty={
        query.isError ? (
          <PageState
            variant="failed"
            size="compact"
            icon={MessageSquare}
            title="Could not load feedback"
            description={failure(query.error)}
            action={<Button onClick={() => void query.refetch()}>Retry</Button>}
          />
        ) : (
          <div className="space-y-4">
            <PageState
              variant="not-set-up"
              size="compact"
              icon={MessageSquare}
              title={
                status === 'open'
                  ? 'No open feedback'
                  : 'No feedback matches this filter'
              }
              requirement="Send a report using the Sentry SDK and your project’s Temps DSN."
              example="A user reports a checkout problem, with the error and replay attached for investigation."
              settingsHref={`/projects/${project.slug}/errors/setup`}
              settingsLabel="Configure SDK ingestion"
            />
            <CodeBlock
              language="typescript"
              code={`Sentry.captureFeedback({\n  message: 'The checkout button did not work',\n  associatedEventId: Sentry.lastEventId(),\n});\nawait Sentry.flush(5000);`}
            />
          </div>
        )
      }
      pagination={
        query.data
          ? {
              page,
              pageSize: 20,
              total: query.data.total,
              totalPages: Math.max(1, Math.ceil(query.data.total / 20)),
              onPageChange: (next) => update('page', String(next)),
            }
          : undefined
      }
    />
  )
}

function FeedbackRecord({ project }: { project: ProjectResponse }) {
  const { feedbackId } = useParams()
  const path = { project_id: project.id, feedback_id: Number(feedbackId) }
  const query = useQuery(getProjectFeedbackOptions({ path }))
  const cache = useQueryClient()
  const navigate = useNavigate()
  const [deleting, setDeleting] = useState(false)
  const invalidate = () => {
    void cache.invalidateQueries({
      queryKey: getProjectFeedbackOptions({ path }).queryKey,
    })
    void cache.invalidateQueries({
      queryKey: listProjectFeedbackOptions({ path: { project_id: project.id } })
        .queryKey,
    })
  }
  const triage = useMutation({
    mutationFn: (status: string) =>
      updateProjectFeedback({ path, body: { status }, throwOnError: true }),
    onSuccess: invalidate,
  })
  const remove = useMutation({
    mutationFn: () => deleteProjectFeedback({ path, throwOnError: true }),
    onError: () => setDeleting(false),
    onSuccess: () => {
      invalidate()
      navigate(base(project))
    },
  })
  if (!query.data)
    return query.isError ? (
      <PageState
        variant="failed"
        size="compact"
        icon={MessageSquare}
        title="Could not load feedback"
        description={failure(query.error)}
        action={<Button onClick={() => void query.refetch()}>Retry</Button>}
      />
    ) : (
      <p role="status">Loading feedback…</p>
    )
  const report = query.data
  return (
    <>
      <Detail
        title="User feedback"
        verdict={verdict(report.status)}
        description={
          <Link className="underline" to={base(project)}>
            All feedback
          </Link>
        }
        actions={
          <>
            <RefreshButton queryKeys={[getProjectFeedbackQueryKey({ path })]} />
            <Button
              variant="outline"
              busy={triage.isPending}
              busyLabel="Updating…"
              onClick={() =>
                triage.mutate(report.status === 'open' ? 'resolved' : 'open')
              }
            >
              {report.status === 'open' ? 'Resolve' : 'Reopen'}
            </Button>
            {report.status !== 'ignored' && (
              <Button
                variant="outline"
                aria-disabled={triage.isPending}
                onClick={() => {
                  if (!triage.isPending) triage.mutate('ignored')
                }}
              >
                Ignore
              </Button>
            )}
            <Button variant="destructive" onClick={() => setDeleting(true)}>
              Delete
            </Button>
          </>
        }
        facts={[
          { label: 'From', value: report.name ?? 'Anonymous' },
          { label: 'Contact', value: report.email ?? 'Not supplied' },
          { label: 'Received', value: stamp(report.received_at) },
          { label: 'Release', value: report.release ?? 'Not supplied' },
          { label: 'Environment', value: report.environment ?? 'Not supplied' },
          {
            label: 'Scope',
            value: report.environment_id
              ? `Environment #${report.environment_id}`
              : 'Project',
          },
        ]}
        main={
          <div className="space-y-6">
            {(triage.isError || remove.isError) && (
              <Callout tone="error" title="Could not update feedback">
                {failure(triage.error ?? remove.error)}
              </Callout>
            )}
            <section className="space-y-2">
              <h2 className="text-base font-semibold">Report</h2>
              <p className="whitespace-pre-wrap break-words text-sm">
                {report.message}
              </p>
              {report.email && (
                <p className="break-all text-sm text-muted-foreground">
                  Contact: {report.email}
                </p>
              )}
              {report.url && (
                <p className="break-all text-sm text-muted-foreground">
                  Page: {report.url}
                </p>
              )}
            </section>
            <section className="space-y-2">
              <h2 className="text-base font-semibold">Investigation</h2>
              {report.error_group_id ? (
                <Link
                  className="block text-sm underline"
                  to={`/projects/${project.slug}/errors/${report.error_group_id}`}
                >
                  View associated error
                </Link>
              ) : (
                <p className="text-sm text-muted-foreground">
                  {report.associated_event_id
                    ? 'The associated error has not arrived or is no longer retained.'
                    : 'No error was attached.'}
                </p>
              )}
              {report.replay_session_id && report.replay_visitor_id ? (
                <Link
                  className="block text-sm underline"
                  to={`/projects/${project.slug}/analytics/visitors/${report.replay_visitor_id}/session-replay/${report.replay_session_id}`}
                >
                  Watch associated replay
                </Link>
              ) : (
                <p className="text-sm text-muted-foreground">
                  {report.replay_id
                    ? 'The associated replay has not arrived or is no longer retained.'
                    : 'No replay was attached.'}
                </p>
              )}
            </section>
            <details>
              <summary className="cursor-pointer text-sm font-medium">
                SDK context
              </summary>
              <ScrollRegion
                className="mt-2 max-h-80"
                regionLabel="Feedback SDK context"
              >
                <pre className="whitespace-pre-wrap break-words text-xs">
                  {JSON.stringify(report.context, null, 2)}
                </pre>
              </ScrollRegion>
            </details>
          </div>
        }
      />
      <ConfirmDeleteDialog
        open={deleting}
        onOpenChange={setDeleting}
        resourceType="feedback report"
        resourceName={`#${report.id}`}
        consequences="This permanently deletes this report and its contact information. Its associated error and replay remain available."
        requireTypedName
        confirmLabel="Delete feedback"
        isPending={remove.isPending}
        onConfirm={() => remove.mutate()}
      />
    </>
  )
}

export default function ProjectFeedback({
  project,
}: {
  project: ProjectResponse
}) {
  return (
    <Routes>
      <Route index element={<FeedbackList project={project} />} />
      <Route
        path=":feedbackId"
        element={<FeedbackRecord project={project} />}
      />
    </Routes>
  )
}
