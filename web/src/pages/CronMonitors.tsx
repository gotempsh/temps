// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'
import {
  Link,
  Route,
  Routes,
  useNavigate,
  useParams,
  useSearchParams,
} from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { CalendarCheck, Clock3, RefreshCw } from 'lucide-react'
import {
  Button,
  Callout,
  DataTable,
  Detail,
  Field,
  FormErrors,
  PageContainer,
  PageHeader,
  PageState,
  RecordLink,
  Settings,
  Status,
  type StatusTone,
} from '@temps-sdk/ds'
import {
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@temps-sdk/ui'
import type {
  CronMonitorView,
  MonitorConfig,
  ProjectResponse,
} from '@/api/client'
import {
  createCronMonitor,
  deleteCronMonitor,
  updateCronMonitor,
} from '@/api/client'
import {
  getCronMonitorOptions,
  getNotificationDeliveryCoverageOptions,
  getEnvironmentsOptions,
  listCronMonitorsOptions,
  listCronMonitorRunsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { CodeBlock } from '@/components/ui/code-block'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { readFailureServerDetail } from '@/lib/read-failure'

const stamp = (value?: string | null) =>
  value ? new Date(value).toLocaleString() : '—'
const basePath = (project: ProjectResponse) =>
  `/projects/${project.slug}/check-ins`
function failure(error: unknown) {
  return (
    readFailureServerDetail(error) ??
    'Temps could not complete this request. Please retry.'
  )
}
function Health({ value }: { value: string }) {
  const tone: StatusTone =
    value === 'ok'
      ? 'ok'
      : ['error', 'failing', 'missed', 'timeout'].includes(value)
        ? 'error'
        : value === 'in_progress'
          ? 'running'
          : value === 'unconfigured'
            ? 'warn'
            : 'idle'
  return <Status tone={tone} label={value.replace(/_/g, ' ')} />
}
function quantity(value: number | undefined, noun: string) {
  return value === undefined
    ? 'Not configured'
    : `${value} ${noun}${value === 1 ? '' : 's'}`
}
function schedule(config?: MonitorConfig | null) {
  if (!config) return 'Schedule required'
  return config.schedule.type === 'crontab'
    ? `${config.schedule.value} (${config.timezone})`
    : `Every ${quantity(config.schedule.value, config.schedule.unit)}`
}
function usePage() {
  const [params, setParams] = useSearchParams()
  const value = Number(params.get('page') ?? 1)
  const page = Number.isSafeInteger(value) && value > 0 ? value : 1
  return {
    page,
    setPage: (next: number) =>
      setParams((previous) => {
        const p = new URLSearchParams(previous)
        p.set('page', String(next))
        return p
      }),
  }
}
function Setup({
  project,
  monitor,
}: {
  project: ProjectResponse
  monitor?: CronMonitorView
}) {
  const coverage = useQuery({
    ...getNotificationDeliveryCoverageOptions({
      query: { severity: 'warning' },
    }),
    retry: false,
  })
  const slug = monitor?.slug ?? 'daily-summary'
  return (
    <section className="space-y-3" aria-label="Check-in setup">
      <h2 className="text-base font-semibold">Send check-ins from your job</h2>
      {coverage.data && !coverage.data.configured && (
        <Callout tone="warning" title="Notifications need configuration">
          {coverage.data.reason}. Native alarms are available in Temps.{' '}
          <Link
            className="underline"
            to={coverage.data.setup_path ?? '/settings/notifications'}
          >
            Configure delivery
          </Link>{' '}
          to receive notifications outside the console.
        </Callout>
      )}
      {coverage.isError && (
        <p className="text-sm text-muted-foreground">
          Could not check notification delivery. An administrator can review{' '}
          <Link className="underline" to="/settings/notifications">
            notification settings
          </Link>
          .
        </p>
      )}
      <p className="text-sm text-muted-foreground">
        Use your project’s Temps DSN in the Sentry SDK. The monitor slug and DSN
        environment must match this job. An unscoped DSN uses the project scope;
        the SDK environment label does not change ownership.
      </p>
      <CodeBlock
        language="typescript"
        code={`import * as Sentry from '@sentry/node';\n\nSentry.init({ dsn: process.env.SENTRY_DSN });\nawait Sentry.withMonitor(${JSON.stringify(slug)}, async () => {\n  await runScheduledJob();\n});\nawait Sentry.flush(5000);`}
      />
      <p className="text-sm text-muted-foreground">
        Schedule this code in your existing scheduler. Temps detects missed
        starts and runtime timeouts; it does not execute the job.
      </p>
      <div className="flex flex-wrap gap-3 text-sm">
        <Link
          className="underline underline-offset-4"
          to={`/projects/${project.slug}/errors/setup`}
        >
          Configure Sentry DSN
        </Link>
        <Link
          className="underline underline-offset-4"
          to="/settings/notifications"
        >
          Notification destinations
        </Link>
        <Link className="underline underline-offset-4" to="/monitoring/alarms">
          Native alarms
        </Link>
      </div>
    </section>
  )
}
/**
 * Monitoring › Check-ins: jobs scheduled outside Temps report each run through
 * the Sentry SDK, and Temps alerts when one fails, starts late or runs too
 * long. Jobs Temps runs itself are listed under Cron jobs.
 */
function CronList({ project }: { project: ProjectResponse }) {
  const { page, setPage } = usePage()
  const query = useQuery({
    ...listCronMonitorsOptions({
      path: { project_id: project.id },
      query: { page, page_size: 20 },
    }),
    refetchInterval: 15000,
  })
  const rows = query.data?.items ?? []
  const empty = !query.isPending && rows.length === 0
  return (
    <PageContainer>
      <PageHeader
        title="Check-ins"
        description="Get alerted when a job you schedule outside Temps fails, starts late or runs too long. The job reports each run through the Sentry SDK."
        actions={
          rows.length > 0 ? (
            <>
              <Button
                variant="outline"
                busy={query.isFetching}
                busyLabel="Refreshing…"
                onClick={() => void query.refetch()}
              >
                <RefreshCw className="size-4" />
                Refresh
              </Button>
              <Button asChild>
                <Link to={`${basePath(project)}/new`}>Create monitor</Link>
              </Button>
            </>
          ) : undefined
        }
      />
      {query.isError && rows.length > 0 && (
        <Callout tone="error" title="Could not refresh check-in monitors">
          {failure(query.error)}
        </Callout>
      )}
      {empty ? (
        query.isError ? (
          <PageState
            variant="failed"
            size="compact"
            icon={Clock3}
            title="Could not load check-in monitors"
            description={failure(query.error)}
            action={<Button onClick={() => void query.refetch()}>Retry</Button>}
          />
        ) : page > 1 ? (
          <PageState
            variant="empty"
            size="compact"
            icon={Clock3}
            title="No monitors on this page"
            action={<Button onClick={() => setPage(1)}>First page</Button>}
          />
        ) : (
          <>
            <PageState
              variant="empty"
              size="compact"
              icon={CalendarCheck}
              title="No check-in monitors yet"
              description="Wrap the job in Sentry.withMonitor and its first check-in creates the monitor, or create it now to catch a job that never starts."
              action={
                <Button asChild>
                  <Link to={`${basePath(project)}/new`}>Create monitor</Link>
                </Button>
              }
            />
            <Setup project={project} />
          </>
        )
      ) : (
        <DataTable<CronMonitorView>
          rows={rows}
          rowKey={(m) => m.id}
          isLoading={query.isPending}
          columns={[
            {
              key: 'slug',
              header: 'Job',
              render: (m) => (
                <RecordLink to={`${basePath(project)}/${m.id}`}>
                  {m.slug}
                </RecordLink>
              ),
            },
            {
              key: 'status',
              header: 'Health',
              render: (m) => <Health value={m.status} />,
            },
            {
              key: 'scope',
              header: 'Scope',
              render: (m) =>
                m.environment_id
                  ? `Environment #${m.environment_id}`
                  : 'Project',
            },
            {
              key: 'schedule',
              header: 'Schedule',
              render: (m) => schedule(m.config),
            },
            {
              key: 'last',
              header: 'Last check-in',
              render: (m) =>
                m.last_check_in_at
                  ? stamp(m.last_check_in_at)
                  : 'Never received',
            },
            {
              key: 'next',
              header: 'Next expected',
              render: (m) => stamp(m.next_expected_at),
            },
          ]}
          pagination={
            query.data
              ? {
                  page,
                  pageSize: 20,
                  total: query.data.total,
                  totalPages: Math.max(1, Math.ceil(query.data.total / 20)),
                  onPageChange: setPage,
                }
              : undefined
          }
        />
      )}
    </PageContainer>
  )
}
function CronRecord({ project }: { project: ProjectResponse }) {
  const { monitorId = '' } = useParams()
  const id = Number(monitorId)
  const { page, setPage } = usePage()
  const path = { project_id: project.id, monitor_id: id }
  const navigate = useNavigate()
  const cache = useQueryClient()
  const [deleting, setDeleting] = useState(false)
  const query = useQuery({
    ...getCronMonitorOptions({ path }),
    refetchInterval: 15000,
  })
  const runs = useQuery({
    ...listCronMonitorRunsOptions({ path, query: { page, page_size: 20 } }),
    refetchInterval: 15000,
  })
  const toggle = useMutation({
    mutationFn: (enabled: boolean) =>
      updateCronMonitor({ path, body: { enabled }, throwOnError: true }),
    onSuccess: () => {
      void cache.invalidateQueries({
        queryKey: getCronMonitorOptions({ path }).queryKey,
      })
      void cache.invalidateQueries({
        queryKey: listCronMonitorsOptions({ path: { project_id: project.id } })
          .queryKey,
      })
    },
  })
  const remove = useMutation({
    mutationFn: () => deleteCronMonitor({ path, throwOnError: true }),
    onError: () => setDeleting(false),
    onSuccess: () => {
      void cache.invalidateQueries({
        queryKey: listCronMonitorsOptions({ path: { project_id: project.id } })
          .queryKey,
      })
      navigate(basePath(project))
    },
  })
  if (!query.data || query.isError)
    return query.isError ? (
      <PageState
        variant="failed"
        size="compact"
        icon={Clock3}
        title="Could not load check-in monitor"
        description={failure(query.error)}
        action={<Button onClick={() => void query.refetch()}>Retry</Button>}
      />
    ) : (
      <p role="status">Loading check-in monitor…</p>
    )
  const monitor = query.data
  return (
    <>
      <Detail
        title={monitor.slug}
        verdict={<Health value={monitor.status} />}
        description={
          <Link className="underline underline-offset-4" to={basePath(project)}>
            All check-ins
          </Link>
        }
        actions={
          <>
            <Button asChild variant="outline">
              <Link to={`${basePath(project)}/${id}/edit`}>
                Edit configuration
              </Link>
            </Button>
            <Button
              variant="outline"
              busy={toggle.isPending}
              busyLabel="Updating…"
              onClick={() => toggle.mutate(!monitor.enabled)}
            >
              {monitor.enabled ? 'Pause monitoring' : 'Resume monitoring'}
            </Button>
            <Button variant="destructive" onClick={() => setDeleting(true)}>
              Delete
            </Button>
          </>
        }
        facts={[
          {
            label: 'Scope',
            value: monitor.environment_id
              ? `Environment #${monitor.environment_id}`
              : 'Project',
          },
          {
            label: 'Last check-in',
            value: monitor.last_check_in_at
              ? stamp(monitor.last_check_in_at)
              : 'Never received',
          },
          { label: 'Next expected', value: stamp(monitor.next_expected_at) },
          {
            label: 'Runtime limit',
            value: monitor.config
              ? quantity(monitor.config.max_runtime, 'minute')
              : 'Not configured',
          },
          { label: 'Failures in a row', value: monitor.consecutive_failures },
          {
            label: 'Configuration',
            value:
              monitor.configuration_source === 'temps'
                ? 'Managed in Temps'
                : 'SDK supplied',
          },
        ]}
        main={
          <>
            {!monitor.enabled && (
              <Callout tone="info" title="Monitoring paused">
                Check-ins are still recorded. Missed runs, timeouts, and new
                alarms are suspended. Resume starts a fresh schedule and health
                window.
              </Callout>
            )}
            {!monitor.config && (
              <Callout tone="warning" title="Schedule required">
                Check-ins are stored, but deadlines cannot be monitored until
                you{' '}
                <Link
                  className="underline"
                  to={`${basePath(project)}/${id}/edit`}
                >
                  configure a schedule
                </Link>
                .
              </Callout>
            )}
            {(toggle.isError || remove.isError) && (
              <Callout tone="error" title="Could not update monitor">
                {failure(toggle.error ?? remove.error)}
              </Callout>
            )}
            <section className="space-y-3">
              <div className="flex items-center justify-between gap-3">
                <h2 className="text-base font-semibold">Run history</h2>
                <Button
                  variant="outline"
                  size="sm"
                  busy={runs.isFetching}
                  busyLabel="Refreshing…"
                  onClick={() =>
                    void Promise.all([query.refetch(), runs.refetch()])
                  }
                >
                  Refresh
                </Button>
              </div>
              {runs.isError ? (
                <PageState
                  variant="failed"
                  size="compact"
                  icon={Clock3}
                  title="Could not load runs"
                  description={failure(runs.error)}
                  action={
                    <Button onClick={() => void runs.refetch()}>Retry</Button>
                  }
                />
              ) : !runs.isPending && !runs.data?.items.length ? (
                <PageState
                  variant="empty"
                  size="compact"
                  icon={Clock3}
                  title={page > 1 ? 'No runs on this page' : 'No runs yet'}
                  description="The first SDK check-in or missed deadline will appear here."
                  action={
                    page > 1 ? (
                      <Button onClick={() => setPage(1)}>First page</Button>
                    ) : undefined
                  }
                />
              ) : (
                <DataTable
                  rows={runs.data?.items ?? []}
                  rowKey={(r) => r.id}
                  isLoading={runs.isPending}
                  columns={[
                    {
                      key: 'status',
                      header: 'Result',
                      render: (r) => <Health value={r.status} />,
                    },
                    {
                      key: 'start',
                      header: 'Started / expected',
                      render: (r) => stamp(r.started_at),
                    },
                    {
                      key: 'finished',
                      header: 'Finished / deadline',
                      render: (r) =>
                        r.status === 'in_progress' && !monitor.enabled
                          ? 'Monitoring paused'
                          : r.status === 'in_progress'
                            ? r.deadline_at
                              ? `Due ${stamp(r.deadline_at)}`
                              : 'Schedule required'
                            : stamp(r.finished_at),
                    },
                    {
                      key: 'duration',
                      header: 'Duration',
                      render: (r) =>
                        r.duration != null ? `${r.duration.toFixed(3)} s` : '—',
                    },
                    {
                      key: 'release',
                      header: 'Release',
                      render: (r) => r.release ?? '—',
                    },
                    {
                      key: 'trace',
                      header: 'Trace',
                      render: (r) =>
                        r.trace_id ? (
                          <Link
                            className="underline underline-offset-4"
                            to={`/projects/${project.slug}/traces/${r.trace_id}`}
                          >
                            View trace
                          </Link>
                        ) : (
                          '—'
                        ),
                    },
                  ]}
                  pagination={
                    runs.data
                      ? {
                          page,
                          pageSize: 20,
                          total: runs.data.total,
                          totalPages: Math.max(
                            1,
                            Math.ceil(runs.data.total / 20)
                          ),
                          onPageChange: setPage,
                        }
                      : undefined
                  }
                />
              )}
              <p className="text-xs text-muted-foreground">
                Up to 10,000 runs per monitor, retained for 30 days. Missed
                starts are coalesced after downtime. Deadline checks run every
                15 seconds.
              </p>
            </section>
            <Setup project={project} monitor={monitor} />
          </>
        }
        aside={
          <section className="space-y-3 text-sm">
            <h2 className="font-semibold">Monitoring policy</h2>
            <p className="break-words font-mono text-xs">
              {schedule(monitor.config)}
            </p>
            {monitor.config && (
              <>
                <p>
                  Start grace:{' '}
                  {quantity(monitor.config.checkin_margin, 'minute')}
                </p>
                <p>
                  Alert after{' '}
                  {quantity(
                    monitor.config.failure_issue_threshold,
                    'failed run'
                  )}{' '}
                  in a row. Recover after{' '}
                  {quantity(
                    monitor.config.recovery_threshold,
                    'successful run'
                  )}
                  .
                </p>
              </>
            )}
            <p className="text-muted-foreground">
              Schedules edited here override SDK configuration. Slug and scope
              stay fixed so SDK check-ins keep reaching the same monitor.
            </p>
            {monitor.alarm_id && (
              <Link className="inline-block underline" to="/monitoring/alarms">
                View native alarms
              </Link>
            )}
          </section>
        }
      />
      <ConfirmDeleteDialog
        open={deleting}
        onOpenChange={setDeleting}
        resourceType="check-in monitor"
        resourceName={monitor.slug}
        consequences="This removes the monitor and all its run history. Future SDK check-ins can recreate it. Stop sending them first to remove it permanently."
        requireTypedName
        confirmLabel="Delete monitor"
        isPending={remove.isPending}
        onConfirm={() => remove.mutate()}
      />
    </>
  )
}
const positiveInteger = (max: number) =>
  z
    .string()
    .refine(
      (value) =>
        /^\d+$/.test(value) &&
        Number.isSafeInteger(Number(value)) &&
        Number(value) >= 1 &&
        Number(value) <= max,
      `Enter a whole number from 1 to ${max}.`
    )
const cronFormSchema = z
  .object({
    slug: z
      .string()
      .regex(
        /^[A-Za-z0-9_-]{1,128}$/,
        'Enter 1–128 letters, digits, underscores or hyphens.'
      ),
    environment: z
      .string()
      .refine(
        (value) =>
          value === 'project' ||
          (/^\d+$/.test(value) &&
            Number(value) > 0 &&
            Number(value) <= 2147483647),
        'Choose a valid environment.'
      ),
    kind: z.enum(['crontab', 'interval']),
    cron: z.string(),
    interval: z.string(),
    unit: z.enum(['minute', 'hour', 'day', 'week']),
    timezone: z
      .string()
      .min(1)
      .max(128)
      .refine((value) => {
        try {
          new Intl.DateTimeFormat('en', { timeZone: value })
          return true
        } catch {
          return false
        }
      }, 'Enter a valid IANA timezone.'),
    margin: positiveInteger(40320),
    runtime: positiveInteger(40320),
    failures: positiveInteger(100),
    recoveries: positiveInteger(100),
  })
  .superRefine((values, context) => {
    if (values.kind === 'interval') {
      const parsed = positiveInteger(525600).safeParse(values.interval)
      if (!parsed.success)
        context.addIssue({
          code: 'custom',
          path: ['interval'],
          message: 'Enter a whole number from 1 to 525600.',
        })
    } else if (
      values.cron.length > 128 ||
      values.cron.trim().split(/\s+/).length !== 5
    ) {
      context.addIssue({
        code: 'custom',
        path: ['cron'],
        message:
          'Enter a five-field crontab expression of at most 128 characters.',
      })
    }
  })
type CronFormValues = z.infer<typeof cronFormSchema>

function CronForm({
  project,
  monitor,
}: {
  project: ProjectResponse
  monitor?: CronMonitorView
}) {
  const navigate = useNavigate()
  const cache = useQueryClient()
  const environments = useQuery(
    getEnvironmentsOptions({ path: { project_id: project.id } })
  )
  const existing = monitor?.config
  const form = useForm<CronFormValues>({
    resolver: zodResolver(cronFormSchema),
    defaultValues: {
      slug: monitor?.slug ?? '',
      environment: String(monitor?.environment_id ?? 'project'),
      kind: existing?.schedule.type ?? 'crontab',
      cron:
        existing?.schedule.type === 'crontab'
          ? existing.schedule.value
          : '0 * * * *',
      interval:
        existing?.schedule.type === 'interval'
          ? String(existing.schedule.value)
          : '1',
      unit:
        existing?.schedule.type === 'interval'
          ? (existing.schedule.unit as CronFormValues['unit'])
          : 'hour',
      timezone: existing?.timezone ?? 'UTC',
      margin: String(existing?.checkin_margin ?? 1),
      runtime: String(existing?.max_runtime ?? 30),
      failures: String(existing?.failure_issue_threshold ?? 1),
      recoveries: String(existing?.recovery_threshold ?? 1),
    },
  })
  const [kind, environment, unit] = useWatch({
    control: form.control,
    name: ['kind', 'environment', 'unit'],
  })
  const error = form.formState.errors.root?.server?.message
  const save = useMutation({
    mutationFn: async (values: CronFormValues) => {
      const config: MonitorConfig = {
        schedule:
          values.kind === 'crontab'
            ? { type: 'crontab', value: values.cron }
            : {
                type: 'interval',
                value: Number(values.interval),
                unit: values.unit,
              },
        timezone: values.timezone,
        checkin_margin: Number(values.margin),
        max_runtime: Number(values.runtime),
        failure_issue_threshold: Number(values.failures),
        recovery_threshold: Number(values.recoveries),
      }
      if (monitor) {
        const result = await updateCronMonitor({
          path: { project_id: project.id, monitor_id: monitor.id },
          body: { config },
          throwOnError: true,
        })
        return result.data
      }
      const result = await createCronMonitor({
        path: { project_id: project.id },
        body: {
          slug: values.slug,
          environment_id:
            values.environment === 'project'
              ? null
              : Number(values.environment),
          config,
        },
        throwOnError: true,
      })
      return result.data
    },
    onSuccess: (data) => {
      void cache.invalidateQueries({
        queryKey: listCronMonitorsOptions({ path: { project_id: project.id } })
          .queryKey,
      })
      cache.setQueryData(
        getCronMonitorOptions({
          path: { project_id: project.id, monitor_id: data.id },
        }).queryKey,
        data
      )
      navigate(`${basePath(project)}/${data.id}`)
    },
    onError: (e) => form.setError('root.server', { message: failure(e) }),
  })
  const submit = form.handleSubmit((values) => {
    form.clearErrors('root')
    save.mutate(values)
  })
  const numeric = (
    label: string,
    name: 'margin' | 'runtime' | 'failures' | 'recoveries' | 'interval',
    max: number
  ) => (
    <Field label={label} error={form.formState.errors[name]?.message}>
      {(props) => (
        <Input
          {...props}
          type="number"
          min={1}
          max={max}
          step={1}
          required
          {...form.register(name)}
        />
      )}
    </Field>
  )
  return (
    <Settings
      title={monitor ? `Configure ${monitor.slug}` : 'Create check-in monitor'}
      description="Configure the schedule before the first run to detect a job that never starts."
      onSubmit={submit}
      saving={save.isPending}
      onCancel={() =>
        navigate(
          monitor ? `${basePath(project)}/${monitor.id}` : basePath(project)
        )
      }
    >
      {error && (
        <Callout tone="error" title="Could not save monitor">
          {error}
        </Callout>
      )}
      <FormErrors
        errors={Object.fromEntries(
          Object.entries(form.formState.errors)
            .filter(([key]) => key !== 'root')
            .map(([key, value]) => [key, value?.message])
        )}
      />
      <div className="grid max-w-3xl gap-5 sm:grid-cols-2">
        <Field
          label="Monitor slug"
          description="Match the monitorSlug used by your SDK. Letters, digits, underscores, and hyphens; fixed after creation."
        >
          {(props) => (
            <Input
              {...props}
              required
              pattern="[A-Za-z0-9_-]+"
              maxLength={128}
              readOnly={!!monitor}
              {...form.register('slug')}
            />
          )}
        </Field>
        <Field
          label="Scope"
          description="Match the DSN’s environment scope. Fixed after creation."
        >
          {(props) =>
            monitor ? (
              <Input
                {...props}
                readOnly
                value={
                  monitor.environment_id
                    ? `Environment #${monitor.environment_id}`
                    : 'Project (unscoped DSN)'
                }
              />
            ) : (
              <Select
                value={environment}
                onValueChange={(value) => form.setValue('environment', value)}
              >
                <SelectTrigger {...props}>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="project">
                    Project (unscoped DSN)
                  </SelectItem>
                  {environments.data?.map((e) => (
                    <SelectItem key={e.id} value={String(e.id)}>
                      {e.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )
          }
        </Field>
        {!monitor && environments.isError && (
          <Callout tone="warning" title="Environment list unavailable">
            Project scope is available.{' '}
            <Button
              type="button"
              variant="link"
              onClick={() => void environments.refetch()}
            >
              Retry environments
            </Button>
          </Callout>
        )}
        <Field label="Schedule type">
          {(props) => (
            <Select
              value={kind}
              onValueChange={(v) =>
                form.setValue('kind', v as CronFormValues['kind'])
              }
            >
              <SelectTrigger {...props}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="crontab">Crontab</SelectItem>
                <SelectItem value="interval">Interval</SelectItem>
              </SelectContent>
            </Select>
          )}
        </Field>
        {kind === 'crontab' ? (
          <Field
            label="Crontab expression"
            description="Five fields: minute, hour, day, month, weekday."
          >
            {(props) => (
              <Input
                {...props}
                required
                maxLength={128}
                {...form.register('cron')}
              />
            )}
          </Field>
        ) : (
          <>
            {numeric('Interval', 'interval', 525600)}
            <Field label="Interval unit">
              {(props) => (
                <Select
                  value={unit}
                  onValueChange={(value) =>
                    form.setValue('unit', value as CronFormValues['unit'])
                  }
                >
                  <SelectTrigger {...props}>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {['minute', 'hour', 'day', 'week'].map((u) => (
                      <SelectItem key={u} value={u}>
                        {u}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              )}
            </Field>
          </>
        )}
        <Field
          label="Timezone"
          description="IANA timezone, for example UTC or Europe/Madrid."
        >
          {(props) => (
            <Input
              {...props}
              required
              maxLength={128}
              {...form.register('timezone')}
            />
          )}
        </Field>
        {numeric('Start grace (minutes)', 'margin', 40320)}
        {numeric('Maximum runtime (minutes)', 'runtime', 40320)}
        {numeric('Failures before alert', 'failures', 100)}
        {numeric('Successes before recovery', 'recoveries', 100)}
      </div>
      <Callout tone="info" title="Managed in Temps">
        Saving makes this configuration authoritative over SDK settings and
        resets the health counters. Paused monitors stay paused. Configure
        notification destinations in{' '}
        <Link className="underline" to="/settings/notifications">
          Settings → Notifications
        </Link>{' '}
        for delivery beyond native alarms.
      </Callout>
    </Settings>
  )
}
function CronEdit({ project }: { project: ProjectResponse }) {
  const { monitorId = '' } = useParams()
  const query = useQuery(
    getCronMonitorOptions({
      path: { project_id: project.id, monitor_id: Number(monitorId) },
    })
  )
  if (!query.data || query.isError)
    return query.isError ? (
      <PageState
        variant="failed"
        size="compact"
        icon={Clock3}
        title="Could not load monitor"
        description={failure(query.error)}
        action={<Button onClick={() => void query.refetch()}>Retry</Button>}
      />
    ) : (
      <p role="status">Loading configuration…</p>
    )
  return <CronForm key={query.data.id} project={project} monitor={query.data} />
}
export default function CronMonitors({
  project,
}: {
  project: ProjectResponse
}) {
  return (
    <Routes>
      <Route index element={<CronList project={project} />} />
      <Route path="new" element={<CronForm project={project} />} />
      <Route path=":monitorId" element={<CronRecord project={project} />} />
      <Route path=":monitorId/edit" element={<CronEdit project={project} />} />
    </Routes>
  )
}
