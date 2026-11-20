// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Callout,
  EchoDialog,
  Field,
  Ledger,
  PageState,
  RecordLink,
  Status,
} from '@temps-sdk/ds'
import { EllipsisVertical, Play, Workflow } from 'lucide-react'
import { Link, useNavigate } from 'react-router'
import { useState } from 'react'
import { Controller, useForm } from 'react-hook-form'
import { z } from 'zod'
import { zodResolver } from '@hookform/resolvers/zod'
import { toast } from 'sonner'
import type {
  ProjectResponse,
  WorkflowSummaryResponse,
  WorkflowsOverviewResponse,
} from '@/api/client'
import { getWorkflowsOverviewOptions } from '@/api/client/@tanstack/react-query.gen'
import { startWorkflowRun, issueWorkflowCredential } from '@/api/client/sdk.gen'
import { useProjectEnvironment } from '@/components/project-frame/project-environment-context'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'
import { RefreshButton } from '@/components/ui/refresh-button'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { problemDetail } from '@/lib/api-problem'
import { runHref, workflowHref } from './status'
import { WorkflowsSetupGuide } from './WorkflowsSetupGuide'

const schema = z.object({
  workflow: z
    .string()
    .min(1, 'Choose a workflow')
    .max(128)
    .regex(/^[a-zA-Z0-9._:/-]+$/),
  input: z.string().refine((value) => {
    try {
      const parsed = JSON.parse(value)
      return (
        parsed !== null && typeof parsed === 'object' && !Array.isArray(parsed)
      )
    } catch {
      return false
    }
  }, 'Enter a JSON object, for example {"orderId":1}'),
})
type Fields = z.infer<typeof schema>

export function EnvironmentWorkflows({
  project,
}: {
  project: ProjectResponse
}) {
  const bar = useProjectEnvironment()
  const environment = bar.selection.environment
  if (!environment)
    return (
      <PageState
        variant={bar.isError ? 'failed' : 'empty'}
        size="compact"
        icon={Workflow}
        title="Choose an environment"
        description="Select an environment to configure and run durable workflows."
      />
    )
  return (
    <WorkflowEnvironment
      key={environment.id}
      projectId={project.id}
      environmentId={environment.id}
    />
  )
}

export function WorkflowEnvironment({
  projectId,
  environmentId,
}: {
  projectId: number
  environmentId: number
}) {
  const path = { project_id: projectId, environment_id: environmentId }
  const options = getWorkflowsOverviewOptions({ path })
  const query = useQuery(options)
  const cache = useQueryClient()
  const [credential, setCredential] = useState<string | null>(null)
  const [rotating, setRotating] = useState(false)
  const [starting, setStarting] = useState<string | null>(null)
  const overview = query.data
  const workflows = overview?.workflows ?? []
  const available = workflows.filter((workflow) => workflow.defined)
  const issue = useMutation({
    mutationFn: async () =>
      (await issueWorkflowCredential({ path, throwOnError: true })).data,
    onSuccess: async (result) => {
      setCredential(result.token)
      setRotating(false)
      toast.success('Workflow credential created')
      await cache.invalidateQueries({ queryKey: options.queryKey })
    },
    onError: (error) =>
      toast.error(problemDetail(error, 'Could not create the credential.')),
  })
  const issueCredential = () => {
    if (issue.isPending) return
    if (overview?.credential.configured) setRotating(true)
    else issue.mutate()
  }
  const base = `/workflows/${projectId}/${environmentId}`

  return (
    <>
      <Ledger<WorkflowSummaryResponse>
        title="Durable workflows"
        rows={workflows}
        rowKey={(row) => row.name}
        isLoading={query.isPending}
        actions={
          <div className="flex items-center gap-2">
            {available.length > 0 && (
              <Button onClick={() => setStarting(available[0]?.name ?? '')}>
                <Play className="size-4" />
                Start run
              </Button>
            )}
            <RefreshButton queryKeys={[options.queryKey]} />
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="outline" size="icon">
                  <EllipsisVertical className="size-4" />
                  <span className="sr-only">Workflow settings and access</span>
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem asChild>
                  <Link to={`${base}/settings`}>Settings</Link>
                </DropdownMenuItem>
                <DropdownMenuItem asChild>
                  <Link to={`${base}/grants`}>Access grants</Link>
                </DropdownMenuItem>
                <DropdownMenuItem asChild>
                  <Link to={`/workflows/${projectId}/keys`}>
                    Encryption keys
                  </Link>
                </DropdownMenuItem>
                <DropdownMenuItem asChild>
                  <Link to="/workflows?view=runs">Runs in every project</Link>
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem
                  onSelect={issueCredential}
                  aria-disabled={issue.isPending}
                >
                  {overview?.credential.configured
                    ? 'Rotate credential'
                    : 'Create credential'}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        }
        toolbar={
          <>
            {credential && (
              <IssuedCredential
                token={credential}
                onDismiss={() => setCredential(null)}
              />
            )}
            {overview && workflows.length > 0 && (
              <EnvironmentStatus
                overview={overview}
                settingsHref={`${base}/settings`}
              />
            )}
          </>
        }
        columns={[
          {
            key: 'name',
            header: 'Workflow',
            render: (row) => (
              <div className="min-w-0">
                <RecordLink
                  to={workflowHref({
                    project_id: projectId,
                    environment_id: environmentId,
                    name: row.name,
                  })}
                >
                  {row.name}
                </RecordLink>
                {(row.version || row.concurrency) && (
                  <p className="text-xs text-muted-foreground">
                    {[
                      row.version && `Version ${row.version}`,
                      row.concurrency && `${row.concurrency} at once`,
                    ]
                      .filter(Boolean)
                      .join(' · ')}
                  </p>
                )}
              </div>
            ),
          },
          {
            key: 'state',
            header: 'State',
            render: (row) => (
              <Status
                tone={row.defined ? 'ok' : 'warn'}
                label={row.defined ? 'Deployed' : 'Not in deployment'}
              />
            ),
          },
          {
            key: 'active',
            header: 'Active',
            className: 'hidden @xl:table-cell',
            render: (row) => row.active_runs,
          },
          {
            key: 'completed',
            header: 'Completed',
            className: 'hidden @xl:table-cell',
            render: (row) => row.completed_runs,
          },
          {
            key: 'failed',
            header: 'Failed',
            className: 'hidden @xl:table-cell',
            render: (row) => row.failed_runs,
          },
          {
            key: 'last',
            header: 'Last run',
            render: (row) =>
              row.last_run_at ? (
                <TimeAgo date={row.last_run_at} />
              ) : (
                <span className="text-muted-foreground">Never</span>
              ),
          },
          {
            key: 'actions',
            header: <span className="sr-only">Actions</span>,
            className: 'text-right',
            render: (row) =>
              row.defined ? (
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => setStarting(row.name)}
                >
                  <Play className="size-4" />
                  <span className="max-sm:sr-only">Run</span>
                  <span className="sr-only"> {row.name}</span>
                </Button>
              ) : null,
          },
        ]}
        empty={
          query.isError ? (
            <PageState
              variant="failed"
              size="compact"
              icon={Workflow}
              title="Could not load workflows"
              description={problemDetail(
                query.error,
                'The server did not answer.'
              )}
              action={
                <Button
                  variant="outline"
                  aria-disabled={query.isFetching}
                  onClick={() => {
                    if (!query.isFetching) void query.refetch()
                  }}
                >
                  {query.isFetching ? 'Trying again…' : 'Try again'}
                </Button>
              }
            />
          ) : (
            <WorkflowsSetupGuide
              title="Connect your app to durable workflows"
              target={{ projectId, environmentId }}
              credentialConfigured={overview?.credential.configured}
              executorReady={overview?.executor.configured}
              executorReason={overview?.executor.reason}
              credentialAction={
                <Button
                  variant={
                    overview?.credential.configured ? 'outline' : 'default'
                  }
                  aria-disabled={issue.isPending}
                  onClick={issueCredential}
                >
                  {issue.isPending
                    ? 'Creating…'
                    : overview?.credential.configured
                      ? 'Rotate credential'
                      : 'Create credential'}
                </Button>
              }
            />
          )
        }
      />
      <StartRunDialog
        open={starting !== null}
        initialWorkflow={starting ?? ''}
        workflows={available.map((workflow) => workflow.name)}
        executorReady={overview?.executor.configured ?? false}
        path={path}
        onOpenChange={(open) => {
          if (!open) setStarting(null)
        }}
      />
      <EchoDialog
        open={rotating}
        onOpenChange={setRotating}
        title="Rotate workflow credential?"
        description="The old credential stops working immediately. Update your app with the new credential."
        phrase="rotate"
        confirmLabel="Rotate credential"
        busy={issue.isPending}
        onConfirm={() => {
          if (!issue.isPending) issue.mutate()
        }}
      />
    </>
  )
}

/** One line saying whether Temps can reach the app and which credential it uses. */
function EnvironmentStatus({
  overview,
  settingsHref,
}: {
  overview: WorkflowsOverviewResponse
  settingsHref: string
}) {
  const { executor, credential } = overview
  return (
    <div className="space-y-3">
      <dl className="flex flex-col gap-2 text-sm sm:flex-row sm:flex-wrap sm:items-center sm:gap-x-6">
        <div className="flex items-center gap-2">
          <dt className="sr-only">Executor</dt>
          <dd className="flex items-center gap-2">
            <Status
              tone={executor.configured ? 'ok' : 'warn'}
              label={
                executor.configured
                  ? 'Executor reachable'
                  : 'Executor not reachable'
              }
              variant="dot"
            />
            {executor.configured && (
              <span className="text-muted-foreground">
                {[
                  executor.sdk,
                  `${executor.replicas} ${executor.replicas === 1 ? 'replica' : 'replicas'}`,
                ]
                  .filter(Boolean)
                  .join(' · ')}
              </span>
            )}
          </dd>
        </div>
        <div className="flex items-center gap-2">
          <dt className="sr-only">Credential</dt>
          <dd className="flex items-center gap-2">
            <Status
              tone={credential.configured ? 'ok' : 'warn'}
              label={credential.configured ? 'Credential' : 'No credential'}
              variant="dot"
            />
            {credential.configured && (
              <span className="text-muted-foreground">
                <code className="font-mono">{credential.prefix}…</code>
                {credential.last_used_at ? (
                  <>
                    {' · used '}
                    <TimeAgo date={credential.last_used_at} />
                  </>
                ) : (
                  ' · never used'
                )}
              </span>
            )}
          </dd>
        </div>
      </dl>
      {!executor.configured && (
        <Callout
          tone="warning"
          title="Runs stay queued until Temps can reach your app"
        >
          <span className="[overflow-wrap:anywhere]">
            {executor.reason ?? 'Deploy an app that serves its workflows.'}{' '}
          </span>
          <Link className="underline underline-offset-4" to={settingsHref}>
            Executor settings
          </Link>
        </Callout>
      )}
    </div>
  )
}

function IssuedCredential({
  token,
  onDismiss,
}: {
  token: string
  onDismiss: () => void
}) {
  return (
    <Callout
      tone="success"
      title="Copy the credential now; it is not shown again"
    >
      <div className="space-y-2">
        <p>
          Set it as the secret environment variable{' '}
          <code className="font-mono">TEMPS_WORKFLOWS_TOKEN</code>.
        </p>
        <code className="block font-mono [overflow-wrap:anywhere]">
          {token}
        </code>
        <div className="flex flex-wrap gap-2">
          <CopyButton value={token} />
          <Button variant="outline" size="sm" onClick={onDismiss}>
            Dismiss
          </Button>
        </div>
      </div>
    </Callout>
  )
}

function StartRunDialog({
  open,
  initialWorkflow,
  workflows,
  executorReady,
  path,
  onOpenChange,
}: {
  open: boolean
  initialWorkflow: string
  workflows: string[]
  executorReady: boolean
  path: { project_id: number; environment_id: number }
  onOpenChange: (open: boolean) => void
}) {
  const navigate = useNavigate()
  const form = useForm<Fields>({
    resolver: zodResolver(schema),
    values: { workflow: initialWorkflow, input: '{}' },
  })
  const start = useMutation({
    mutationFn: async (value: Fields) =>
      (
        await startWorkflowRun({
          path,
          body: {
            workflow: value.workflow,
            input: JSON.parse(value.input),
            idempotency_key: crypto.randomUUID(),
          },
          throwOnError: true,
        })
      ).data,
    onSuccess: (result) => {
      toast.success('Workflow run started')
      onOpenChange(false)
      navigate(runHref(result.run))
    },
  })
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Start a run</DialogTitle>
        </DialogHeader>
        <form
          id="start-workflow-run"
          className="space-y-4"
          onSubmit={form.handleSubmit((value) => {
            if (!start.isPending) start.mutate(value)
          })}
        >
          <Field
            label="Workflow"
            error={form.formState.errors.workflow?.message}
          >
            {(props) => (
              <Controller
                name="workflow"
                control={form.control}
                render={({ field }) => (
                  <Select value={field.value} onValueChange={field.onChange}>
                    <SelectTrigger
                      {...props}
                      onBlur={field.onBlur}
                      ref={field.ref}
                    >
                      <SelectValue placeholder="Select a workflow" />
                    </SelectTrigger>
                    <SelectContent>
                      {workflows.map((workflow) => (
                        <SelectItem key={workflow} value={workflow}>
                          {workflow}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                )}
              />
            )}
          </Field>
          <Field
            label="Input (JSON)"
            error={form.formState.errors.input?.message}
          >
            {(props) => (
              <Textarea
                {...props}
                {...form.register('input')}
                className="min-h-28 font-mono"
              />
            )}
          </Field>
          {!executorReady && (
            <p className="text-sm text-muted-foreground">
              Temps cannot reach your app yet; the run stays queued until it
              can.
            </p>
          )}
          {start.isError && (
            <p role="alert" className="text-sm text-destructive">
              {problemDetail(start.error, 'Could not start the workflow.')}
            </p>
          )}
        </form>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button
            type="submit"
            form="start-workflow-run"
            aria-disabled={start.isPending}
          >
            {start.isPending ? 'Starting…' : 'Start run'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
