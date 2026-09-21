// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { Link, useSearchParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import {
  CheckCircle2,
  CircleAlert,
  CircleHelp,
  Clock3,
  PauseCircle,
  History,
  ShieldCheck,
} from 'lucide-react'
import {
  listVariableHistory,
  type EnvironmentVariableResponse,
  type VerificationResult,
} from '@/api/client'
import { PageHeader } from '@/components/layout/PageContainer'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { EmptyState } from '@/components/ui/empty-state'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { Tabs, TabsList, TabsTrigger, TabsContent } from '@temps-sdk/ds'
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from '@/components/ui/table'
import { useHttpChecks } from './http-checks'

const eventNames: Record<string, string> = {
  tracking_started: 'History tracking started',
  detection_unavailable: 'Credential could not be read for automatic detection',
  created: 'Variable created',
  value_changed: 'Value rotated',
  settings_changed: 'Variable settings updated',
  check_added: 'Check added',
  check_updated: 'Check updated',
  check_removed: 'Check removed',
  check_paused: 'Check paused',
  check_resumed: 'Check resumed',
  verification: 'Verification completed',
}

const statuses = {
  healthy: {
    label: 'Healthy',
    icon: CheckCircle2,
    color: 'text-emerald-700 dark:text-emerald-400',
  },
  warning: {
    label: 'Warning',
    icon: CircleAlert,
    color: 'text-amber-700 dark:text-amber-400',
  },
  error: { label: 'Failed', icon: CircleAlert, color: 'text-destructive' },
  unknown: {
    label: 'Unknown',
    icon: CircleHelp,
    color: 'text-muted-foreground',
  },
  pending: { label: 'Pending', icon: Clock3, color: 'text-muted-foreground' },
  paused: {
    label: 'Paused',
    icon: PauseCircle,
    color: 'text-muted-foreground',
  },
}
function Status({ status }: { status: keyof typeof statuses }) {
  const { label, icon: Icon, color } = statuses[status]
  return (
    <span
      className={`inline-flex items-center gap-1.5 whitespace-nowrap text-sm font-medium ${color}`}
    >
      <Icon aria-hidden="true" className="size-4 shrink-0" />
      {label}
    </span>
  )
}
function Timestamp({
  value,
  compact = false,
}: {
  value: string | number
  compact?: boolean
}) {
  const date = new Date(value)
  return (
    <time
      dateTime={date.toISOString()}
      title={date.toLocaleString()}
      className={
        compact
          ? 'inline-flex items-center gap-2 whitespace-nowrap tabular-nums text-sm'
          : 'whitespace-nowrap tabular-nums text-sm'
      }
    >
      <span className="block">
        {date.toLocaleDateString(undefined, {
          month: 'short',
          day: 'numeric',
          year: 'numeric',
        })}
      </span>
      <span className="block text-xs text-muted-foreground">
        {date.toLocaleTimeString(undefined, {
          hour: '2-digit',
          minute: '2-digit',
        })}
      </span>
    </time>
  )
}
function Findings({
  result,
  label,
}: {
  result?: VerificationResult | null
  label?: string
}) {
  if (!result?.findings.length) return null
  return (
    <details
      className={
        label
          ? 'text-sm text-muted-foreground'
          : 'mt-1 text-sm text-muted-foreground'
      }
    >
      <summary className="w-fit cursor-pointer rounded-sm py-1 hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring">
        {label ? (
          <>
            <span className="font-medium text-foreground">{label}</span>
            <span className="ml-2 text-xs">
              {result.findings.length}{' '}
              {result.findings.length === 1 ? 'finding' : 'findings'}
            </span>
          </>
        ) : (
          `View findings (${result.findings.length})`
        )}
      </summary>
      <ul role="list" className="space-y-1 py-2">
        {result.findings.map((finding, index) => (
          <li
            key={`${finding.code}-${index}`}
            className="max-w-prose whitespace-normal break-words"
          >
            {finding.message}
          </li>
        ))}
      </ul>
    </details>
  )
}
export function EnvironmentVariableDetails({
  projectId,
  variable,
  detailPath,
}: {
  projectId: number
  variable: EnvironmentVariableResponse
  detailPath: string
}) {
  const [params, setParams] = useSearchParams()
  const tab = params.get('tab') === 'history' ? 'history' : 'checks'
  const pageNumber = (key: string) => {
    const value = Number(params.get(key) ?? 1)
    return Number.isSafeInteger(value) && value > 0 ? value : 1
  }
  const page = pageNumber('page')
  const updateParam = (key: string, value: string) =>
    setParams((previous) => {
      const next = new URLSearchParams(previous)
      if (value === '1' || (key === 'tab' && value === 'checks'))
        next.delete(key)
      else next.set(key, value)
      return next
    })
  const checks = useHttpChecks(projectId)
  const priority = {
    error: 0,
    warning: 1,
    unknown: 2,
    pending: 3,
    healthy: 4,
    paused: 5,
  }
  const scoped = (checks.data ?? [])
    .filter((check) => check.env_var_id === variable.id)
    .sort(
      (a, b) =>
        priority[a.enabled ? (a.result?.status ?? 'pending') : 'paused'] -
          priority[b.enabled ? (b.result?.status ?? 'pending') : 'paused'] ||
        a.id - b.id
    )
  const issues = scoped.filter(
    (check) =>
      check.enabled && ['error', 'warning'].includes(check.result?.status ?? '')
  ).length
  const checkPage = Math.min(
    pageNumber('checksPage'),
    Math.max(1, Math.ceil(scoped.length / 10))
  )
  const history = useQuery({
    queryKey: ['variable-history', projectId, variable.id, page, 15],
    queryFn: async () =>
      (
        await listVariableHistory({
          path: { project_id: projectId, env_var_id: variable.id },
          query: { page, page_size: 15 },
          throwOnError: true,
        })
      ).data,
    enabled: tab === 'history',
    refetchInterval: tab === 'history' && page === 1 ? 5000 : false,
  })
  const configure = (
    <Button variant="outline" size="sm" asChild>
      <Link to={`${detailPath}/checks`}>Configure checks</Link>
    </Button>
  )
  return (
    <section aria-label="Variable details" className="w-full min-w-0 space-y-6">
      <PageHeader
        title={<span className="font-mono break-all">{variable.key}</span>}
        description="Credential health and variable activity."
        verdict={
          <Badge variant="secondary">
            {variable.is_secret ? 'Secret' : 'Regular'}
          </Badge>
        }
        actions={configure}
      />
      <dl className="grid grid-cols-2 gap-x-6 gap-y-4 border-b pb-5 text-sm lg:grid-cols-4">
        <div>
          <dt className="text-muted-foreground">Value</dt>
          <dd className="mt-1 font-mono">••••••••••••</dd>
          <dd className="text-xs text-muted-foreground">
            {variable.is_secret ? 'Secret · write-only' : 'Hidden in this view'}
          </dd>
        </div>
        <div>
          <dt className="text-muted-foreground">Environments</dt>
          <dd className="mt-1 flex flex-wrap gap-1">
            {variable.environments.length
              ? variable.environments.map((env) => (
                  <Badge key={env.id} variant="outline">
                    {env.name}
                  </Badge>
                ))
              : 'None'}
          </dd>
        </div>
        <div>
          <dt className="text-muted-foreground">Check health</dt>
          <dd className="mt-1">
            {checks.isError ? (
              'Unavailable'
            ) : checks.isPending ? (
              'Loading…'
            ) : issues ? (
              <span className="font-medium text-destructive">
                {issues} {issues === 1 ? 'check needs' : 'checks need'}{' '}
                attention
              </span>
            ) : scoped.length ? (
              `${scoped.filter((c) => c.enabled && c.result?.status === 'healthy').length} of ${scoped.length} healthy`
            ) : (
              'No checks configured'
            )}
          </dd>
        </div>
        <div>
          <dt className="text-muted-foreground">Created</dt>
          <dd className="mt-1">
            <Timestamp value={variable.created_at} />
          </dd>
        </div>
      </dl>
      <Tabs value={tab} onValueChange={(value) => updateParam('tab', value)}>
        <TabsList aria-label="Variable views">
          <TabsTrigger
            value="checks"
            count={checks.data ? scoped.length : undefined}
          >
            Checks
          </TabsTrigger>
          <TabsTrigger value="history">History</TabsTrigger>
        </TabsList>
        <TabsContent value="checks" className="mt-5 space-y-4">
          <div>
            <h3 className="font-medium">Verification checks</h3>
            <p className="mt-1 text-sm text-muted-foreground">
              Checks needing attention appear first. Recognized credentials are
              verified daily.
            </p>
          </div>
          <div className="overflow-hidden rounded-lg border bg-card">
            {checks.isError ? (
              <div role="alert" className="p-6 text-sm">
                Could not load checks.{' '}
                <Button variant="link" onClick={() => void checks.refetch()}>
                  Retry
                </Button>
              </div>
            ) : checks.isPending ? (
              <p role="status" className="p-6 text-sm text-muted-foreground">
                Loading checks…
              </p>
            ) : !scoped.length ? (
              <EmptyState
                size="compact"
                icon={ShieldCheck}
                title="No checks configured"
                description="Automatic detection runs after creation and rotation. Custom or self-hosted credentials need an endpoint."
              />
            ) : (
              <Table aria-label="Verification checks">
                <TableHeader>
                  <TableRow className="bg-muted/40">
                    <TableHead>Check</TableHead>
                    <TableHead>Status</TableHead>
                    <TableHead>Schedule</TableHead>
                    <TableHead>Last verified</TableHead>
                    <TableHead>Next run</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {scoped
                    .slice((checkPage - 1) * 10, checkPage * 10)
                    .map((check) => (
                      <TableRow key={check.id}>
                        <TableCell className="min-w-56 whitespace-normal">
                          <p className="font-medium">{check.name}</p>
                          <p className="mt-1 text-xs text-muted-foreground">
                            {check.automatic_provider
                              ? 'Automatic detection'
                              : 'Custom HTTP check'}
                          </p>
                          <Findings result={check.result} />
                        </TableCell>
                        <TableCell className="align-top py-4">
                          <Status
                            status={
                              !check.enabled
                                ? 'paused'
                                : (check.result?.status ?? 'pending')
                            }
                          />
                        </TableCell>
                        <TableCell className="align-top py-4 whitespace-nowrap">
                          {check.interval_seconds === 86400
                            ? 'Daily'
                            : `Every ${check.interval_seconds / 3600} hours`}
                        </TableCell>
                        <TableCell className="align-top py-4">
                          {check.last_checked_at ? (
                            <Timestamp value={check.last_checked_at} />
                          ) : (
                            <span className="text-muted-foreground">
                              Not run yet
                            </span>
                          )}
                        </TableCell>
                        <TableCell className="align-top py-4">
                          {check.enabled ? (
                            <Timestamp value={check.next_check_at} />
                          ) : (
                            <span className="text-muted-foreground">
                              Paused
                            </span>
                          )}
                        </TableCell>
                      </TableRow>
                    ))}
                </TableBody>
              </Table>
            )}
          </div>
          {scoped.length > 10 && (
            <ResponsivePagination
              page={checkPage}
              pageSize={10}
              total={scoped.length}
              totalPages={Math.ceil(scoped.length / 10)}
              onPageChange={(value) => updateParam('checksPage', String(value))}
              ariaLabel="Checks pagination"
            />
          )}
        </TabsContent>
        <TabsContent value="history" className="mt-5 space-y-4">
          <div className="flex flex-wrap items-start justify-between gap-2">
            <div>
              <h3 className="font-medium">Activity history</h3>
              <p className="mt-1 text-sm text-muted-foreground">
                Verification runs and changes, newest first. Secret values are
                never recorded.
              </p>
            </div>
            <p className="text-xs text-muted-foreground">
              Times in {Intl.DateTimeFormat().resolvedOptions().timeZone}
            </p>
          </div>
          <div className="overflow-hidden rounded-lg border bg-card">
            {history.isPending ? (
              <p role="status" className="p-6 text-sm text-muted-foreground">
                Loading history…
              </p>
            ) : history.isError ? (
              <div role="alert" className="p-6 text-sm">
                Could not load history.{' '}
                <Button variant="link" onClick={() => void history.refetch()}>
                  Retry
                </Button>
              </div>
            ) : !history.data.items.length ? (
              <EmptyState
                size="compact"
                icon={History}
                title="No activity on this page"
                description="Verification runs and variable changes appear here as they happen."
              />
            ) : (
              <Table aria-label="Variable activity">
                <TableHeader>
                  <TableRow className="bg-muted/40">
                    <TableHead className="w-56">Time</TableHead>
                    <TableHead>Event</TableHead>
                    <TableHead>Check</TableHead>
                    <TableHead className="w-36">Result</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {history.data.items.map((event) => {
                    const details = event.details as {
                      check_name?: string
                      result?: VerificationResult
                    }
                    return (
                      <TableRow key={event.id}>
                        <TableCell className="align-top py-2">
                          <Timestamp value={event.created_at} compact />
                        </TableCell>
                        <TableCell className="min-w-52 align-top py-2 whitespace-normal">
                          {details.result?.findings.length ? (
                            <Findings
                              result={details.result}
                              label={
                                eventNames[event.kind] ?? 'Variable activity'
                              }
                            />
                          ) : (
                            <p className="py-1 font-medium">
                              {eventNames[event.kind] ?? 'Variable activity'}
                            </p>
                          )}
                        </TableCell>
                        <TableCell className="min-w-48 align-top py-2 whitespace-normal text-muted-foreground">
                          {details.check_name || (
                            <span aria-label="Not applicable">—</span>
                          )}
                        </TableCell>
                        <TableCell className="align-top py-2">
                          {details.result ? (
                            <Status status={details.result.status} />
                          ) : (
                            <span
                              className="text-muted-foreground"
                              aria-label="Not applicable"
                            >
                              —
                            </span>
                          )}
                        </TableCell>
                      </TableRow>
                    )
                  })}
                </TableBody>
              </Table>
            )}
          </div>
          {history.data &&
          (history.data.total > history.data.page_size || page > 1) ? (
            <ResponsivePagination
              page={page}
              pageSize={history.data.page_size}
              total={history.data.total}
              totalPages={Math.max(
                1,
                Math.ceil(history.data.total / history.data.page_size)
              )}
              onPageChange={(value) => updateParam('page', String(value))}
              ariaLabel="History pagination"
            />
          ) : history.data ? (
            <p className="text-xs text-muted-foreground tabular-nums">
              {history.data.total}{' '}
              {history.data.total === 1 ? 'event' : 'events'}
            </p>
          ) : null}
        </TabsContent>
      </Tabs>
    </section>
  )
}
