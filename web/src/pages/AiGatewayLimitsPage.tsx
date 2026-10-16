// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getLimitsOverview,
  getProjectLimits,
  getProjects,
  getUsageByProject,
  resetProjectLimits,
  updateInstanceLimits,
  updateProjectLimits,
} from '@/api/client'
import type {
  LimitSettingsBody,
  ProjectLimitOverride,
} from '@/api/client/types.gen'
import { AiLimitDialog } from '@/components/ai/AiLimitDialog'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { EmptyState } from '@/components/ui/empty-state'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { Label } from '@/components/ui/label'
import { Progress } from '@/components/ui/progress'
import { ReadFailure } from '@/components/ui/read-failure'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { SearchableSelect } from '@/components/ui/searchable-select'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  type LimitFormValues,
  type LimitPayload,
  describeInstanceBudget,
  describeInstanceRequestLimit,
  budgetUsedPercent,
  formatBudgetLimit,
  formatMicrocents,
  formatRequestLimit,
  limitsToFormValues,
  monthStartIso,
} from '@/lib/ai-gateway-limits'
import { extractProblemDetails } from '@/utils/errorHandling'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Gauge, Pencil, Plus, RotateCcw } from 'lucide-react'
import { useMemo, useState } from 'react'
import { toast } from 'sonner'

const OVERRIDES_PAGE_SIZE = 20
const PROJECT_PICKER_SIZE = 100
const SPEND_ROWS = 50

const EMPTY_LIMITS: LimitFormValues = {
  requestsPerMinute: '',
  monthlyBudgetUsd: '',
}

function hasStoredLimits(limits: LimitSettingsBody): boolean {
  return (
    limits.max_requests_per_minute != null ||
    limits.max_cost_per_month_microcents != null
  )
}

/** Spend against a project's monthly budget, or a note when nothing caps it. */
function BudgetUsage({
  spend,
  budget,
}: {
  spend: number
  budget: number | null
}) {
  const percent = budgetUsedPercent(spend, budget)
  if (percent === null || budget === null) {
    return <span className="text-muted-foreground">No cap</span>
  }
  return (
    <div className="min-w-[8rem] space-y-1">
      <Progress
        value={percent}
        aria-label={`${Math.round(percent)}% of the monthly budget used`}
        className="h-2"
      />
      <p className="text-xs text-muted-foreground">
        {Math.round(percent)}% of {formatMicrocents(budget)}
      </p>
    </div>
  )
}

function mutationMessage(error: unknown, fallback: string): string {
  const problem = extractProblemDetails(error)
  return problem?.detail ?? problem?.title ?? fallback
}

type DialogState =
  | { kind: 'closed' }
  | { kind: 'instance' }
  | { kind: 'add' }
  | { kind: 'edit'; override: ProjectLimitOverride }

export function AiGatewayLimitsPage() {
  usePageTitle('AI Limits')
  const queryClient = useQueryClient()

  const [page, setPage] = useState(1)
  const [dialog, setDialog] = useState<DialogState>({ kind: 'closed' })
  const [pickedProject, setPickedProject] = useState('')
  const [resetting, setResetting] = useState<ProjectLimitOverride | null>(null)

  const overview = useQuery({
    queryKey: ['aiLimits', 'overview', page],
    queryFn: async () => {
      const { data } = await getLimitsOverview({
        query: { page, page_size: OVERRIDES_PAGE_SIZE },
        throwOnError: true,
      })
      return data
    },
    placeholderData: (previous) => previous,
  })

  // The picker only knows the overrides on the current page, so the chosen
  // project's stored limits are fetched on their own. The add form starts from
  // them: saving replaces both limits, and a blank form would silently clear
  // a project that already has an override on another page.
  const pickedId = pickedProject === '' ? null : Number(pickedProject)
  const pickedLimits = useQuery({
    queryKey: ['aiLimits', 'project', pickedId],
    enabled: dialog.kind === 'add' && pickedId !== null,
    queryFn: async () => {
      const { data } = await getProjectLimits({
        path: { project_id: Number(pickedProject) },
        throwOnError: true,
      })
      return data
    },
  })

  const projects = useQuery({
    queryKey: ['aiLimits', 'projects'],
    queryFn: async () => {
      const { data } = await getProjects({
        query: { page: 1, per_page: PROJECT_PICKER_SIZE },
        throwOnError: true,
      })
      return data
    },
  })

  const monthStart = useMemo(() => monthStartIso(new Date()), [])
  const spend = useQuery({
    queryKey: ['aiLimits', 'spend', monthStart],
    queryFn: async () => {
      const { data } = await getUsageByProject({
        query: {
          from: monthStart,
          to: new Date().toISOString(),
          limit: SPEND_ROWS,
        },
        throwOnError: true,
      })
      return data
    },
  })

  const projectNames = useMemo(() => {
    const names = new Map<number, string>()
    for (const project of projects.data?.projects ?? []) {
      names.set(project.id, project.name)
    }
    return names
  }, [projects.data])

  const projectLabel = (projectId: number) =>
    projectNames.get(projectId) ?? `Project #${projectId}`

  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ['aiLimits'] })

  const saveInstance = useMutation({
    mutationFn: async (body: LimitSettingsBody) => {
      await updateInstanceLimits({ body, throwOnError: true })
    },
    onSuccess: () => {
      toast.success('Instance defaults updated')
      setDialog({ kind: 'closed' })
      void invalidate()
    },
    onError: (error) =>
      toast.error(mutationMessage(error, 'Failed to update instance defaults')),
  })

  const saveProject = useMutation({
    mutationFn: async (vars: {
      projectId: number
      body: LimitSettingsBody
    }) => {
      await updateProjectLimits({
        path: { project_id: vars.projectId },
        body: vars.body,
        throwOnError: true,
      })
      return vars.projectId
    },
    onSuccess: (projectId) => {
      toast.success(`Limits saved for ${projectLabel(projectId)}`)
      setDialog({ kind: 'closed' })
      setPickedProject('')
      void invalidate()
    },
    onError: (error) =>
      toast.error(mutationMessage(error, 'Failed to save project limits')),
  })

  const resetProject = useMutation({
    mutationFn: async (projectId: number) => {
      await resetProjectLimits({
        path: { project_id: projectId },
        throwOnError: true,
      })
      return projectId
    },
    onSuccess: (projectId) => {
      toast.success(
        `${projectLabel(projectId)} now inherits the instance defaults`
      )
      setResetting(null)
      void invalidate()
    },
    onError: (error) =>
      toast.error(mutationMessage(error, 'Failed to reset project limits')),
  })

  const data = overview.data
  const total = data?.total_project_overrides ?? 0
  const totalPages = Math.max(1, Math.ceil(total / OVERRIDES_PAGE_SIZE))
  const instance = data?.instance
  const requestLimit =
    data && instance
      ? describeInstanceRequestLimit(
          instance.max_requests_per_minute,
          data.built_in_max_requests_per_minute
        )
      : null
  const budgetLimit = instance
    ? describeInstanceBudget(instance.max_cost_per_month_microcents)
    : null

  const takenProjectIds = new Set(
    (data?.project_overrides ?? []).map((o) => o.project_id)
  )
  const projectOptions = (projects.data?.projects ?? []).map((project) => ({
    value: String(project.id),
    label: project.name,
    keywords: project.slug,
    disabled: takenProjectIds.has(project.id),
  }))

  const closeDialog = (open: boolean) => {
    if (!open) {
      setDialog({ kind: 'closed' })
      setPickedProject('')
    }
  }

  const submitProject = (projectId: number, payload: LimitPayload) =>
    saveProject.mutate({ projectId, body: payload })

  return (
    <PageContainer>
      <PageHeader
        title="AI Limits"
        description="Cap how fast and how much each project can spend through the AI gateway. Callers that exceed a cap get a 429 with a Retry-After header."
      />

      {overview.isError && !data ? (
        <ReadFailure
          resource="AI limits"
          error={overview.error}
          onRetry={() => overview.refetch()}
          retrying={overview.isFetching}
        />
      ) : (
        <>
          <Card>
            <CardHeader className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
              <div>
                <CardTitle>Instance defaults</CardTitle>
                <CardDescription>
                  Apply to every project that has no override of its own.
                </CardDescription>
              </div>
              <Button
                variant="outline"
                size="sm"
                disabled={!data}
                onClick={() => setDialog({ kind: 'instance' })}
              >
                <Pencil className="mr-2 size-4" />
                Edit defaults
              </Button>
            </CardHeader>
            <CardContent>
              {!data || !instance ? (
                <Skeleton className="h-12 w-full" />
              ) : (
                <dl className="grid grid-cols-1 gap-4 sm:grid-cols-2">
                  <div>
                    <dt className="text-xs uppercase tracking-wide text-muted-foreground">
                      Requests per minute
                    </dt>
                    <dd className="mt-1 text-lg font-semibold">
                      {requestLimit?.value}
                    </dd>
                    <dd className="text-xs text-muted-foreground">
                      {requestLimit?.note}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-xs uppercase tracking-wide text-muted-foreground">
                      Monthly budget
                    </dt>
                    <dd className="mt-1 text-lg font-semibold">
                      {budgetLimit?.value}
                    </dd>
                    <dd className="text-xs text-muted-foreground">
                      {budgetLimit?.note}
                    </dd>
                  </div>
                </dl>
              )}
            </CardContent>
          </Card>

          <Card>
            <CardHeader className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
              <div>
                <CardTitle>Project overrides</CardTitle>
                <CardDescription>
                  Projects with their own limits. Everything else follows the
                  instance defaults.
                </CardDescription>
              </div>
              <Button
                size="sm"
                disabled={!data}
                onClick={() => setDialog({ kind: 'add' })}
              >
                <Plus className="mr-2 size-4" />
                Add override
              </Button>
            </CardHeader>
            <CardContent className="space-y-4">
              {!data ? (
                <Skeleton className="h-32 w-full" />
              ) : data.project_overrides.length === 0 ? (
                <EmptyState
                  size="compact"
                  icon={Gauge}
                  title="No project overrides"
                  description="Every project uses the instance defaults. Add an override to give a busy project more headroom, or a risky one a tighter cap."
                  action={
                    <Button
                      size="sm"
                      onClick={() => setDialog({ kind: 'add' })}
                    >
                      <Plus className="mr-2 size-4" />
                      Add override
                    </Button>
                  }
                />
              ) : (
                <div
                  aria-busy={overview.isPlaceholderData}
                  className={overview.isPlaceholderData ? 'opacity-60' : ''}
                >
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>Project</TableHead>
                        <TableHead>Requests</TableHead>
                        <TableHead>Monthly budget</TableHead>
                        <TableHead className="text-right">Actions</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {data.project_overrides.map((override) => (
                        <TableRow key={override.project_id}>
                          <TableCell className="font-medium">
                            {projectLabel(override.project_id)}
                          </TableCell>
                          <TableCell>
                            {formatRequestLimit(
                              override.limits.max_requests_per_minute
                            )}
                          </TableCell>
                          <TableCell>
                            {formatBudgetLimit(
                              override.limits.max_cost_per_month_microcents
                            )}
                          </TableCell>
                          <TableCell className="text-right">
                            <Button
                              variant="ghost"
                              size="icon"
                              aria-label={`Edit limits for ${projectLabel(override.project_id)}`}
                              onClick={() =>
                                setDialog({ kind: 'edit', override })
                              }
                            >
                              <Pencil className="size-4" />
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon"
                              aria-label={`Reset limits for ${projectLabel(override.project_id)}`}
                              onClick={() => setResetting(override)}
                            >
                              <RotateCcw className="size-4" />
                            </Button>
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </div>
              )}
              {data && total > OVERRIDES_PAGE_SIZE && (
                <ResponsivePagination
                  page={page}
                  pageSize={OVERRIDES_PAGE_SIZE}
                  total={total}
                  totalPages={totalPages}
                  onPageChange={setPage}
                  ariaLabel="Project overrides pagination"
                />
              )}
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>Spend this month</CardTitle>
              <CardDescription>
                Operator-funded spend per project since the start of the month
                (UTC), estimated from the model price list, against its monthly
                budget. Requests that used a caller-supplied provider key are
                listed but do not count against the budget.
              </CardDescription>
            </CardHeader>
            <CardContent>
              {spend.isPending ? (
                <Skeleton className="h-32 w-full" />
              ) : spend.isError ? (
                <ReadFailure
                  resource="Project spend"
                  error={spend.error}
                  onRetry={() => spend.refetch()}
                  retrying={spend.isFetching}
                  embedded
                />
              ) : spend.data.length === 0 ? (
                <EmptyState
                  size="compact"
                  icon={Gauge}
                  title="No attributed usage yet"
                  description="Requests made with a project's deployment token show up here, with the project and token recorded on every usage log entry."
                />
              ) : (
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Project</TableHead>
                      <TableHead className="text-right">Requests</TableHead>
                      <TableHead className="text-right">Tokens</TableHead>
                      <TableHead className="text-right">
                        Operator spend
                      </TableHead>
                      <TableHead>Monthly budget</TableHead>
                      <TableHead className="text-right">Errors</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {spend.data.map((row) => (
                      <TableRow key={row.project_id}>
                        <TableCell className="font-medium">
                          {projectLabel(row.project_id)}
                          {row.byok_request_count > 0 && (
                            <Badge variant="secondary" className="ml-2">
                              {row.byok_request_count.toLocaleString('en-US')}{' '}
                              own key
                            </Badge>
                          )}
                        </TableCell>
                        <TableCell className="text-right">
                          {row.request_count.toLocaleString('en-US')}
                        </TableCell>
                        <TableCell className="text-right">
                          {(
                            row.input_tokens + row.output_tokens
                          ).toLocaleString('en-US')}
                        </TableCell>
                        <TableCell className="text-right">
                          {formatMicrocents(row.operator_cost_microcents)}
                        </TableCell>
                        <TableCell>
                          <BudgetUsage
                            spend={row.operator_cost_microcents}
                            budget={row.monthly_budget_microcents ?? null}
                          />
                        </TableCell>
                        <TableCell className="text-right">
                          {row.error_count.toLocaleString('en-US')}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              )}
            </CardContent>
          </Card>
        </>
      )}

      <AiLimitDialog
        open={dialog.kind === 'instance'}
        onOpenChange={closeDialog}
        title="Instance defaults"
        description="Applies to every project without its own override. Blank falls back to the built-in default."
        initialValues={limitsToFormValues(instance ?? {})}
        inheritsFrom="the built-in default"
        submitLabel="Save defaults"
        pending={saveInstance.isPending}
        onSubmit={(payload) => saveInstance.mutate(payload)}
      />

      <AiLimitDialog
        open={dialog.kind === 'add'}
        onOpenChange={closeDialog}
        title="Add project override"
        description="Give one project its own limits."
        initialValues={
          pickedLimits.data
            ? limitsToFormValues(pickedLimits.data.stored)
            : EMPTY_LIMITS
        }
        inheritsFrom="the instance default"
        submitLabel="Save override"
        pending={saveProject.isPending}
        fieldsDisabled={pickedId !== null && pickedLimits.isPending}
        submitDisabled={
          pickedId === null || pickedLimits.isPending || pickedLimits.isError
        }
        onSubmit={(payload) => submitProject(Number(pickedProject), payload)}
      >
        <div className="space-y-2">
          <Label>Project</Label>
          <SearchableSelect
            value={pickedProject}
            onValueChange={setPickedProject}
            options={projectOptions}
            placeholder={
              projects.isPending ? 'Loading projects…' : 'Select a project'
            }
            searchPlaceholder="Search projects…"
            emptyText="No matching projects."
          />
          {(projects.data?.total ?? 0) > PROJECT_PICKER_SIZE && (
            <p className="text-xs text-muted-foreground">
              Showing the first {PROJECT_PICKER_SIZE} of{' '}
              {projects.data?.total.toLocaleString('en-US')} projects. Set
              limits for others through the API (
              <code>PUT /ai/limits/projects/&#123;project_id&#125;</code>).
            </p>
          )}
          {projects.isError && (
            <p className="text-xs text-destructive">
              Could not load projects. Close this dialog and try again.
            </p>
          )}
          {pickedLimits.isError && (
            <p className="text-xs text-destructive">
              Could not load this project&apos;s current limits, so it cannot be
              saved safely. Pick it again to retry.
            </p>
          )}
          {pickedLimits.data && hasStoredLimits(pickedLimits.data.stored) && (
            <p className="text-xs text-muted-foreground">
              This project already has an override. Its current values are
              shown; saving replaces them.
            </p>
          )}
        </div>
      </AiLimitDialog>

      <AiLimitDialog
        open={dialog.kind === 'edit'}
        onOpenChange={closeDialog}
        title={
          dialog.kind === 'edit'
            ? `Limits for ${projectLabel(dialog.override.project_id)}`
            : 'Project limits'
        }
        description="Blank inherits the instance default; 0 removes the cap for this project."
        initialValues={
          dialog.kind === 'edit'
            ? limitsToFormValues(dialog.override.limits)
            : EMPTY_LIMITS
        }
        inheritsFrom="the instance default"
        submitLabel="Save limits"
        pending={saveProject.isPending}
        onSubmit={(payload) => {
          if (dialog.kind === 'edit') {
            submitProject(dialog.override.project_id, payload)
          }
        }}
      />

      <AlertDialog
        open={resetting !== null}
        onOpenChange={(open) =>
          !open && !resetProject.isPending && setResetting(null)
        }
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Reset project limits?</AlertDialogTitle>
            <AlertDialogDescription>
              {resetting
                ? `${projectLabel(resetting.project_id)} will go back to the instance defaults. Its current override (${formatRequestLimit(resetting.limits.max_requests_per_minute)}, ${formatBudgetLimit(resetting.limits.max_cost_per_month_microcents)}) is removed.`
                : ''}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={resetProject.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              disabled={resetProject.isPending}
              onClick={(event) => {
                event.preventDefault()
                if (resetting) resetProject.mutate(resetting.project_id)
              }}
            >
              Reset to defaults
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </PageContainer>
  )
}
