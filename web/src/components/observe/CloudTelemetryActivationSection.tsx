// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Bulk Cloud-telemetry activation (ADR-042 §11).
 *
 * This is the instance-wide home for "switch every project to Temps Cloud, and
 * ship the history you already have". It is **always rendered** — on an
 * unlinked instance the button is visible and disabled with the server's own
 * reason and a link to the Cloud setup page, because a control that disappears
 * when unconfigured is a capability nobody discovers.
 *
 * Three things here are deliberate and easy to get wrong:
 *
 * - The ETA branches on `eta_state`, never on whether `eta_seconds` happens to
 *   be present. "estimating…" is a real answer; a fabricated countdown is not.
 * - `switching` and `backfilling` are rendered distinctly. The switch is
 *   instant and free; the backfill runs for hours and costs egress.
 * - A `failed` project is **not** offered a "revert to local" control. The
 *   backend never rolls the switch back (§7), so new spans are still going to
 *   Cloud and the only honest affordance is a retry of the history.
 */

import {
  cancelBulkActivationJobMutation,
  createBulkActivationJobMutation,
  estimateBulkActivationMutation,
  getBulkActivationJobOptions,
  getBulkActivationJobQueryKey,
  getCurrentBulkActivationJobOptions,
  getCurrentBulkActivationJobQueryKey,
  getProjectsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  BulkActivationEstimateResponse,
  BulkActivationJobProjectResponse,
  BulkActivationJobResponse,
  BulkActivationProjectEstimateResponse,
  EstimateBulkActivationRequest,
} from '@/api/client/types.gen'
import { problemDetail } from '@/api/cloudTelemetry'
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
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Progress } from '@/components/ui/progress'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import {
  etaLabel,
  isInternalConsolePath,
  isJobActive,
  isTerminalJobStatus,
  JOB_STATUS_DETAIL,
  JOB_STATUS_LABELS,
  percentLabel,
  problemStatus,
  problemValue,
  PROJECT_STATUS_CLASSES,
  PROJECT_STATUS_HINTS,
  PROJECT_STATUS_LABELS,
  resolveConsoleProjectPath,
  resumableProjectIds,
  retryableProjectIds,
  skipReasonText,
  throughputLabel,
} from '@/lib/cloud-telemetry-activation'
import { formatBytes } from '@/lib/utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertTriangle,
  ArrowRight,
  CloudUpload,
  Loader2,
  RefreshCw,
  Rocket,
} from 'lucide-react'
import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  useSyncExternalStore,
} from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'

/** The anchor other surfaces deep-link to (`/settings/otel-pipeline#...`). */
export const ACTIVATION_SECTION_ANCHOR = 'cloud-telemetry-activation'

/** Where an operator connects Temps Cloud. Matches `CLOUD_SETUP_PATH`. */
const CLOUD_SETUP_PATH = '/settings/cloud'

/**
 * Remembering the last job across reloads.
 *
 * There is no "list jobs" endpoint: `GET current` returns `null` the moment a
 * job stops. Without this, an operator who reloads the page after an
 * activation finished with failures loses the failure list — and with it the
 * only place the retry lives. The id is not sensitive; a stale one 404s and is
 * dropped.
 */
const LAST_BATCH_STORAGE_KEY = 'temps.cloud-telemetry-activation.last-batch-id'

function readLastBatchId(): string | null {
  try {
    return window.localStorage.getItem(LAST_BATCH_STORAGE_KEY)
  } catch {
    // Private mode / disabled storage. Losing the memory is a degraded
    // experience, never a broken card.
    return null
  }
}

function writeLastBatchId(batchId: string | null): void {
  try {
    if (batchId) window.localStorage.setItem(LAST_BATCH_STORAGE_KEY, batchId)
    else window.localStorage.removeItem(LAST_BATCH_STORAGE_KEY)
  } catch {
    /* see readLastBatchId */
  }
}

/**
 * The remembered id as an external store, subscribed to with
 * `useSyncExternalStore`.
 *
 * A plain `useState` would need an effect to copy the discovered job id into
 * React state, and a ref would not be readable during render. Modelling the
 * memory as what it actually is — a value that lives outside React, in
 * `localStorage` — makes reading it render-safe and writing it a plain
 * side effect.
 */
let rememberedBatchId: string | null | undefined
const rememberedBatchIdListeners = new Set<() => void>()

function getRememberedBatchId(): string | null {
  if (rememberedBatchId === undefined) rememberedBatchId = readLastBatchId()
  return rememberedBatchId
}

function subscribeRememberedBatchId(listener: () => void): () => void {
  rememberedBatchIdListeners.add(listener)
  return () => {
    rememberedBatchIdListeners.delete(listener)
  }
}

function rememberBatchId(batchId: string | null): void {
  if (getRememberedBatchId() === batchId) return
  rememberedBatchId = batchId
  writeLastBatchId(batchId)
  for (const listener of rememberedBatchIdListeners) listener()
}

// ---------------------------------------------------------------------------
// Small presentational pieces
// ---------------------------------------------------------------------------

function ProjectStatusBadge({
  status,
}: {
  status: BulkActivationJobProjectResponse['status']
}) {
  return (
    <Badge
      variant="outline"
      className={`whitespace-nowrap ${PROJECT_STATUS_CLASSES[status]}`}
      title={PROJECT_STATUS_HINTS[status]}
    >
      {PROJECT_STATUS_LABELS[status]}
    </Badge>
  )
}

/**
 * A link to the page that unblocks a project, when the server sent one.
 *
 * `project_not_found` deliberately carries no `setup_path` — there is no page
 * for a project that no longer exists — and nothing is invented in its place.
 */
function SetupLink({
  setupPath,
  slugByProjectId,
  label,
}: {
  setupPath: string | null | undefined
  slugByProjectId: ReadonlyMap<number, string>
  label: string
}) {
  if (!isInternalConsolePath(setupPath)) return null
  const href = resolveConsoleProjectPath(setupPath, slugByProjectId)
  return (
    <Link
      to={href}
      className="inline-flex items-center gap-1 font-medium underline underline-offset-2"
    >
      {label}
      <ArrowRight className="size-3" />
    </Link>
  )
}

function ActivationStat({
  label,
  value,
  hint,
}: {
  label: string
  value: string
  hint?: string
}) {
  return (
    <div className="rounded-md border p-3">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className="mt-1 text-base font-semibold tabular-nums">{value}</p>
      {hint && <p className="mt-0.5 text-xs text-muted-foreground">{hint}</p>}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Job panels
// ---------------------------------------------------------------------------

function JobProgress({
  job,
  projectLabel,
}: {
  job: BulkActivationJobResponse
  projectLabel: (projectId: number) => string
}) {
  const eta = etaLabel(job.eta_state, job.eta_seconds)
  const rate = throughputLabel(job.observed_spans_per_sec)
  const hasPercent =
    typeof job.percent_complete === 'number' &&
    Number.isFinite(job.percent_complete)

  return (
    <div className="space-y-3">
      {/* No bar at all when the server omitted the percentage: a bar frozen at
          0% is exactly what a hang looks like. */}
      {hasPercent ? (
        <Progress value={job.percent_complete ?? 0} className="h-2" />
      ) : (
        <p className="text-xs text-muted-foreground">
          Overall progress is not known yet — this activation has no span
          estimate to measure against.
        </p>
      )}

      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <ActivationStat
          label="Overall"
          value={percentLabel(job.percent_complete)}
          hint={`${job.projects_done} of ${job.projects_total} projects done`}
        />
        <ActivationStat
          label="Spans shipped"
          value={`${job.spans_shipped.toLocaleString()} / ${job.estimated_spans.toLocaleString()}`}
          hint={`${formatBytes(job.bytes_shipped)} of an estimated ${formatBytes(
            job.estimated_bytes
          )}`}
        />
        {/* While the job runs this is an ETA with the rate as context. Once it
            has finished there is no time remaining to report, so the same tile
            reports what the activation actually achieved instead of a dash
            sitting next to a live-sounding rate. */}
        <ActivationStat
          label={eta ? 'Time remaining' : 'Throughput'}
          value={eta ?? rate ?? '—'}
          hint={
            eta
              ? (rate ??
                'Rate is measured once the first batch is acknowledged')
              : 'Average achieved across this activation'
          }
        />
        <ActivationStat
          label="Current project"
          value={
            typeof job.current_project_id === 'number'
              ? projectLabel(job.current_project_id)
              : '—'
          }
          hint={
            typeof job.current_project_id === 'number'
              ? 'Being switched or backfilled right now'
              : 'Between projects'
          }
        />
      </div>
    </div>
  )
}

function JobProjectRow({
  project,
  projectLabel,
  slugByProjectId,
}: {
  project: BulkActivationJobProjectResponse
  projectLabel: (projectId: number) => string
  slugByProjectId: ReadonlyMap<number, string>
}) {
  return (
    <TableRow>
      <TableCell className="font-medium">
        {projectLabel(project.project_id)}
        <span className="ml-1 text-xs text-muted-foreground">
          #{project.project_id}
        </span>
      </TableCell>
      <TableCell>
        <ProjectStatusBadge status={project.status} />
      </TableCell>
      <TableCell className="hidden text-right tabular-nums md:table-cell">
        {project.spans_shipped.toLocaleString()} /{' '}
        {project.estimated_spans.toLocaleString()}
      </TableCell>
      <TableCell className="hidden text-right tabular-nums sm:table-cell">
        {percentLabel(project.percent_complete)}
      </TableCell>
      <TableCell className="min-w-[220px] text-xs text-muted-foreground">
        <ProjectRowReason project={project} slugByProjectId={slugByProjectId} />
      </TableCell>
    </TableRow>
  )
}

function ProjectRowReason({
  project,
  slugByProjectId,
}: {
  project: BulkActivationJobProjectResponse
  slugByProjectId: ReadonlyMap<number, string>
}) {
  if (project.status === 'skipped') {
    return (
      <div className="space-y-1">
        {/* Verbatim server prose. Re-deriving copy from the machine token is
            how a client ends up contradicting the server. */}
        <p>{skipReasonText(project)}</p>
        <SetupLink
          setupPath={project.setup_path}
          slugByProjectId={slugByProjectId}
          label="Fix this"
        />
      </div>
    )
  }

  if (project.status === 'failed') {
    return (
      <div className="space-y-1">
        <p className="font-mono break-words text-destructive">
          {project.last_error ?? 'No reason was recorded.'}
        </p>
        <p>
          History backfill failed; new data is still going to Temps Cloud. The
          switch is never rolled back, so this is a recorded, retryable hole in
          history.
        </p>
        <SetupLink
          setupPath={project.setup_path}
          slugByProjectId={slugByProjectId}
          label="Open project settings"
        />
      </div>
    )
  }

  return <p>{PROJECT_STATUS_HINTS[project.status]}</p>
}

function JobProjectTable({
  projects,
  projectLabel,
  slugByProjectId,
}: {
  projects: BulkActivationJobProjectResponse[]
  projectLabel: (projectId: number) => string
  slugByProjectId: ReadonlyMap<number, string>
}) {
  if (projects.length === 0) return null
  return (
    <div className="overflow-x-auto">
      <Table className="min-w-[560px]">
        <TableHeader>
          <TableRow>
            <TableHead>Project</TableHead>
            <TableHead>State</TableHead>
            <TableHead className="hidden text-right md:table-cell">
              Spans
            </TableHead>
            <TableHead className="hidden text-right sm:table-cell">
              Done
            </TableHead>
            <TableHead>What it means</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {projects.map((project) => (
            <JobProjectRow
              key={project.project_id}
              project={project}
              projectLabel={projectLabel}
              slugByProjectId={slugByProjectId}
            />
          ))}
        </TableBody>
      </Table>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Estimate / confirm dialog
// ---------------------------------------------------------------------------

function EstimateRow({
  row,
  projectLabel,
  slugByProjectId,
}: {
  row: BulkActivationProjectEstimateResponse
  projectLabel: (projectId: number) => string
  slugByProjectId: ReadonlyMap<number, string>
}) {
  return (
    <TableRow className={row.eligible ? undefined : 'text-muted-foreground'}>
      <TableCell className="font-medium">
        {projectLabel(row.project_id)}
        <span className="ml-1 text-xs text-muted-foreground">
          #{row.project_id}
        </span>
      </TableCell>
      <TableCell className="text-right tabular-nums">
        {row.eligible ? row.estimated_spans.toLocaleString() : '—'}
      </TableCell>
      <TableCell className="hidden text-right tabular-nums sm:table-cell">
        {row.eligible ? formatBytes(row.estimated_bytes) : '—'}
      </TableCell>
      <TableCell className="text-xs">
        {row.eligible ? (
          <span className="text-muted-foreground">
            Will be switched and backfilled
          </span>
        ) : (
          <div className="space-y-1">
            <p>{row.skip_detail ?? row.skip_reason ?? 'Not eligible.'}</p>
            <SetupLink
              setupPath={row.setup_path}
              slugByProjectId={slugByProjectId}
              label="Fix this"
            />
          </div>
        )}
      </TableCell>
    </TableRow>
  )
}

function ConfirmActivationDialog({
  open,
  onOpenChange,
  estimate,
  isRefreshedQuote,
  isSubmitting,
  onConfirm,
  projectLabel,
  slugByProjectId,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  estimate: BulkActivationEstimateResponse | null
  isRefreshedQuote: boolean
  isSubmitting: boolean
  onConfirm: () => void
  projectLabel: (projectId: number) => string
  slugByProjectId: ReadonlyMap<number, string>
}) {
  const canConfirm = !!estimate?.plan_token && estimate.eligible_projects > 0

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[85vh] max-w-3xl overflow-y-auto">
        <DialogHeader>
          <DialogTitle>Switch projects to Temps Cloud</DialogTitle>
          <DialogDescription>
            This sends existing history to Temps Cloud, which is metered egress
            and costs money. Nothing is sent until you confirm.
          </DialogDescription>
        </DialogHeader>

        <ConfirmDialogBody
          estimate={estimate}
          isRefreshedQuote={isRefreshedQuote}
          projectLabel={projectLabel}
          slugByProjectId={slugByProjectId}
        />

        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={isSubmitting}
          >
            Cancel
          </Button>
          <Button onClick={onConfirm} disabled={!canConfirm || isSubmitting}>
            {isSubmitting && <Loader2 className="size-4 animate-spin" />}
            {estimate
              ? `Send ${estimate.estimated_spans.toLocaleString()} spans (${formatBytes(
                  estimate.estimated_bytes
                )})`
              : 'Confirm'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function ConfirmDialogBody({
  estimate,
  isRefreshedQuote,
  projectLabel,
  slugByProjectId,
}: {
  estimate: BulkActivationEstimateResponse | null
  isRefreshedQuote: boolean
  projectLabel: (projectId: number) => string
  slugByProjectId: ReadonlyMap<number, string>
}) {
  if (!estimate) {
    return (
      <div className="space-y-2">
        <Skeleton className="h-5 w-52" />
        <Skeleton className="h-24 w-full" />
      </div>
    )
  }

  return (
    <div className="space-y-4">
      {isRefreshedQuote && (
        <Alert>
          <RefreshCw className="h-4 w-4" />
          <AlertTitle>This quote was re-issued</AlertTitle>
          <AlertDescription>
            The previous plan expired before it was submitted, so the estimate
            was re-run. Check the totals below — they may have moved — and
            confirm again.
          </AlertDescription>
        </Alert>
      )}

      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        <ActivationStat
          label="Projects"
          value={`${estimate.eligible_projects}`}
          hint={`${estimate.skipped_projects} skipped of ${estimate.total_projects}`}
        />
        <ActivationStat
          label="Spans"
          value={estimate.estimated_spans.toLocaleString()}
          hint="Exact count in the window"
        />
        <ActivationStat
          label="Metered bytes"
          value={formatBytes(estimate.estimated_bytes)}
          hint="Projected; Cloud's acknowledgement is authoritative"
        />
        <ActivationStat
          label="Window"
          value={new Date(estimate.window_from).toLocaleDateString()}
          hint={`through ${new Date(estimate.window_to).toLocaleDateString()}`}
        />
      </div>

      {estimate.eligible_projects === 0 && (
        <Alert>
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>Nothing is eligible right now</AlertTitle>
          <AlertDescription>
            Every project in this scope was skipped, so there is no bill to
            confirm. Each reason is listed below with the page that unblocks it.
          </AlertDescription>
        </Alert>
      )}

      <div className="overflow-x-auto">
        <Table className="min-w-[520px]">
          <TableHeader>
            <TableRow>
              <TableHead>Project</TableHead>
              <TableHead className="text-right">Spans</TableHead>
              <TableHead className="hidden text-right sm:table-cell">
                Bytes
              </TableHead>
              <TableHead>Notes</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {estimate.projects.map((row) => (
              <EstimateRow
                key={row.project_id}
                row={row}
                projectLabel={projectLabel}
                slugByProjectId={slugByProjectId}
              />
            ))}
          </TableBody>
        </Table>
      </div>

      <p className="text-xs text-muted-foreground">
        Each project is switched to Cloud-primary first — instant, and it sends
        nothing — and its history follows behind. A project whose backfill fails
        stays on Cloud for new data; its history can be retried from this page.
      </p>
    </div>
  )
}

// ---------------------------------------------------------------------------
// The section
// ---------------------------------------------------------------------------

/**
 * Whether an activation could run, and why not.
 *
 * Seeded from the cheap instance status endpoint and then corrected by the
 * estimate endpoint, which is the authority: `POST /estimate` always answers
 * 200 and carries `configured` + `reason` + `setup_path` precisely so this
 * state is renderable without guessing from an error code.
 */
type ActivationCapability = {
  configured: boolean
  reason?: string
  setupPath?: string
}

export function CloudTelemetryActivationSection({
  configured,
  statusPending,
  reason,
  setupPath,
}: {
  /** Whether Temps Cloud can take telemetry writes at all, from the status endpoint. */
  configured: boolean
  /** The status endpoint has not answered yet, so `configured` is not yet meaningful. */
  statusPending?: boolean
  /** The server's own sentence for why it cannot, when it cannot. */
  reason?: string
  /** Where the operator goes to fix that. */
  setupPath?: string
}) {
  const queryClient = useQueryClient()

  const currentJobQuery = useQuery({
    ...getCurrentBulkActivationJobOptions(),
    // Discovery only: a purchase-triggered job is created server-side with no
    // click here, so this has to be asked for rather than waited for.
    refetchInterval: 30_000,
    retry: false,
  })

  const [confirmOpen, setConfirmOpen] = useState(false)
  const [cancelOpen, setCancelOpen] = useState(false)
  const [estimate, setEstimate] =
    useState<BulkActivationEstimateResponse | null>(null)
  const [isRefreshedQuote, setIsRefreshedQuote] = useState(false)
  const [scope, setScope] = useState<EstimateBulkActivationRequest>({
    all_eligible_projects: true,
  })
  const [capability, setCapability] = useState<ActivationCapability | null>(
    null
  )

  // The job to show: whatever is running right now, and otherwise whatever was
  // last on screen. The remembered id is what keeps a job visible after it
  // finishes and `GET current` starts answering `null` — without it a job that
  // completed with failures would vanish at exactly the moment its failure list
  // and its retry matter most.
  const activeBatchId = currentJobQuery.data?.batch_id ?? null
  const lastBatchId = useSyncExternalStore(
    subscribeRememberedBatchId,
    getRememberedBatchId
  )
  const trackedBatchId = activeBatchId ?? lastBatchId

  const jobQuery = useQuery({
    ...getBulkActivationJobOptions({
      path: { batch_id: trackedBatchId ?? '' },
    }),
    enabled: !!trackedBatchId,
    retry: false,
    refetchInterval: (query) =>
      query.state.data && !isTerminalJobStatus(query.state.data.status)
        ? 5_000
        : false,
  })

  // Names make a progress line readable; ids make it unambiguous. Same query
  // key the header already uses, so this costs nothing extra.
  const projectsQuery = useQuery({
    ...getProjectsOptions({ query: { page: 1, per_page: 100 } }),
    staleTime: 60_000,
  })

  // A remembered id from another instance (or a pruned job) is not an error
  // state — it just means there is nothing to show. Only a 404 counts: a 500 or
  // a network blip must not throw the id away, or the card would forget a
  // running job the moment the server hiccupped.
  const jobIsGone = jobQuery.isError && problemStatus(jobQuery.error) === 404

  // Keeping the external memory in step with what is actually on screen. This
  // writes to storage, not to React state, which is exactly what an effect is
  // for — and it is a no-op when the value has not moved, so it cannot loop.
  useEffect(() => {
    rememberBatchId(jobIsGone ? null : trackedBatchId)
  }, [jobIsGone, trackedBatchId])

  const slugByProjectId = useMemo(() => {
    const map = new Map<number, string>()
    for (const project of projectsQuery.data?.projects ?? []) {
      map.set(project.id, project.slug)
    }
    return map
  }, [projectsQuery.data])

  const nameByProjectId = useMemo(() => {
    const map = new Map<number, string>()
    for (const project of projectsQuery.data?.projects ?? []) {
      map.set(project.id, project.name)
    }
    return map
  }, [projectsQuery.data])

  const projectLabel = useCallback(
    (projectId: number) =>
      nameByProjectId.get(projectId) ?? `Project ${projectId}`,
    [nameByProjectId]
  )

  const estimateMutation = useMutation(estimateBulkActivationMutation())
  const createMutation = useMutation(createBulkActivationJobMutation())
  const cancelMutation = useMutation(cancelBulkActivationJobMutation())

  const invalidateJobQueries = useCallback(
    (batchId?: string) => {
      void queryClient.invalidateQueries({
        queryKey: getCurrentBulkActivationJobQueryKey(),
      })
      if (batchId) {
        void queryClient.invalidateQueries({
          queryKey: getBulkActivationJobQueryKey({
            path: { batch_id: batchId },
          }),
        })
      }
    },
    [queryClient]
  )

  /**
   * Quote a scope.
   *
   * Returns `null` when the estimate did not produce something to confirm —
   * either it failed, or the server proactively reported an activation already
   * running, in which case the honest move is to show that job rather than open
   * a dialog whose confirm button would be refused with a 409.
   */
  const requestQuote = useCallback(
    async (
      nextScope: EstimateBulkActivationRequest
    ): Promise<BulkActivationEstimateResponse | null> => {
      setScope(nextScope)
      try {
        const quote = await estimateMutation.mutateAsync({ body: nextScope })

        // The estimate is the capability endpoint. If it says the instance is
        // not set up, that answer replaces whatever the cheaper status read
        // implied — and it is rendered as onboarding, never as a failure.
        setCapability({
          configured: quote.configured,
          reason: quote.reason ?? undefined,
          setupPath: quote.setup_path ?? undefined,
        })
        if (!quote.configured) {
          setConfirmOpen(false)
          setEstimate(null)
          toast.info(
            quote.reason ??
              'Temps Cloud is not set up for telemetry on this instance, so there is nothing to switch to yet.'
          )
          return null
        }

        if (quote.active_batch_id) {
          rememberBatchId(quote.active_batch_id)
          setConfirmOpen(false)
          setEstimate(null)
          invalidateJobQueries(quote.active_batch_id)
          toast.info(
            'A Cloud telemetry activation is already running. Showing that job instead.'
          )
          return null
        }
        return quote
      } catch (error) {
        toast.error(
          problemDetail(
            error,
            'The instance could not quote this activation, so nothing was started.'
          )
        )
        return null
      }
    },
    [estimateMutation, invalidateJobQueries]
  )

  const openConfirm = useCallback(
    async (nextScope: EstimateBulkActivationRequest) => {
      const quote = await requestQuote(nextScope)
      if (!quote) return
      setEstimate(quote)
      setIsRefreshedQuote(false)
      setConfirmOpen(true)
    },
    [requestQuote]
  )

  const submit = useCallback(async () => {
    const planToken = estimate?.plan_token
    if (!planToken) return
    try {
      const created = await createMutation.mutateAsync({
        body: { plan_token: planToken },
      })
      rememberBatchId(created.batch_id)
      setConfirmOpen(false)
      setEstimate(null)
      invalidateJobQueries(created.batch_id)
      toast.success(
        `Activation queued for ${created.projects_total} project${
          created.projects_total === 1 ? '' : 's'
        }. Progress is shown here.`
      )
      return
    } catch (error) {
      const status = problemStatus(error)
      const conflictBatchId = problemValue(error, 'batch_id')

      // 409: not an error the operator can act on. Switch straight to the job
      // that is already running rather than a toast that explains nothing.
      if (status === 409 && conflictBatchId) {
        rememberBatchId(conflictBatchId)
        setConfirmOpen(false)
        setEstimate(null)
        invalidateJobQueries(conflictBatchId)
        toast.info(
          'An activation was already running, so this one was not queued. Showing the running job.'
        )
        return
      }

      // 400 + re_estimate_path: the plan expired between quote and submit.
      // Re-quote the same scope automatically; the operator confirms numbers,
      // not tokens.
      if (status === 400 && problemValue(error, 're_estimate_path')) {
        const fresh = await requestQuote(scope)
        if (fresh) {
          setEstimate(fresh)
          setIsRefreshedQuote(true)
          setConfirmOpen(true)
          toast.info(
            'That quote expired before it was submitted. It has been re-run — confirm the refreshed totals.'
          )
        }
        return
      }

      toast.error(
        problemDetail(
          error,
          'The instance refused to queue this activation, so nothing was sent.'
        )
      )
    }
  }, [createMutation, estimate, invalidateJobQueries, requestQuote, scope])

  // A 404 means the remembered job no longer exists. Rendering nothing is the
  // honest answer; rendering an error would imply something is broken.
  const job = jobIsGone
    ? undefined
    : (jobQuery.data ?? currentJobQuery.data ?? undefined)
  const jobIsActive = isJobActive(job)

  const confirmCancel = useCallback(async () => {
    if (!job) return
    try {
      const updated = await cancelMutation.mutateAsync({
        path: { batch_id: job.batch_id },
      })
      invalidateJobQueries(updated.batch_id)
      toast.success(
        'Cancellation requested. The activation stops at the next chunk boundary; nothing already sent is re-sent when you resume.'
      )
    } catch (error) {
      toast.error(
        problemDetail(
          error,
          'The instance could not record the cancellation. The activation is still running.'
        )
      )
    } finally {
      setCancelOpen(false)
    }
  }, [cancelMutation, invalidateJobQueries, job])

  const startAll = useCallback(
    () => void openConfirm({ all_eligible_projects: true }),
    [openConfirm]
  )

  const startScoped = useCallback(
    (projectIds: number[]) => void openConfirm({ project_ids: projectIds }),
    [openConfirm]
  )

  const isQuoting = estimateMutation.isPending
  const currentJobDenied = problemStatus(currentJobQuery.error) === 403

  return (
    <section
      id={ACTIVATION_SECTION_ANCHOR}
      className="scroll-mt-20 space-y-4 rounded-lg border p-4"
    >
      <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <h3 className="flex items-center gap-2 text-sm font-medium">
            <Rocket className="size-4" aria-hidden="true" />
            Cloud telemetry activation
          </h3>
          <p className="mt-1 text-xs leading-5 text-muted-foreground">
            Switch every eligible project to Cloud-primary writes and ship the
            history this instance is still holding, in one queued job instead of
            one API call and one CLI run per project.
          </p>
        </div>
        {job && (
          <Badge variant={jobIsActive ? 'default' : 'secondary'}>
            {JOB_STATUS_LABELS[job.status]}
          </Badge>
        )}
      </div>

      <ActivationBody
        job={job}
        isLoading={currentJobQuery.isPending && !job}
        isDenied={currentJobDenied}
        projectLabel={projectLabel}
        slugByProjectId={slugByProjectId}
        onCancel={() => setCancelOpen(true)}
        onRetryScoped={startScoped}
      />

      <StartAction
        configured={capability?.configured ?? configured}
        statusPending={!capability && !!statusPending}
        reason={capability?.reason ?? reason}
        setupPath={capability?.setupPath ?? setupPath}
        jobIsActive={jobIsActive}
        isQuoting={isQuoting}
        isDenied={currentJobDenied}
        onStart={startAll}
      />

      <ConfirmActivationDialog
        open={confirmOpen}
        onOpenChange={setConfirmOpen}
        estimate={estimate}
        isRefreshedQuote={isRefreshedQuote}
        isSubmitting={createMutation.isPending || estimateMutation.isPending}
        onConfirm={() => void submit()}
        projectLabel={projectLabel}
        slugByProjectId={slugByProjectId}
      />

      <AlertDialog open={cancelOpen} onOpenChange={setCancelOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Stop this activation?</AlertDialogTitle>
            <AlertDialogDescription>
              Projects already switched stay on Temps Cloud — the switch is
              never rolled back. Spans already shipped are not re-sent if you
              resume later, and the job stops at the next chunk boundary rather
              than mid-batch.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={cancelMutation.isPending}>
              Keep running
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={(event) => {
                event.preventDefault()
                void confirmCancel()
              }}
              disabled={cancelMutation.isPending}
            >
              {cancelMutation.isPending && (
                <Loader2 className="size-4 animate-spin" />
              )}
              Stop activation
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </section>
  )
}

function ActivationBody({
  job,
  isLoading,
  isDenied,
  projectLabel,
  slugByProjectId,
  onCancel,
  onRetryScoped,
}: {
  job: BulkActivationJobResponse | undefined
  isLoading: boolean
  isDenied: boolean
  projectLabel: (projectId: number) => string
  slugByProjectId: ReadonlyMap<number, string>
  onCancel: () => void
  onRetryScoped: (projectIds: number[]) => void
}) {
  if (isDenied) {
    return (
      <Alert>
        <AlertTriangle className="h-4 w-4" />
        <AlertTitle>You cannot see activation jobs on this instance</AlertTitle>
        <AlertDescription>
          Bulk Cloud telemetry activation is restricted to instance
          administrators, so this section cannot show whether one is running.
          Projects can still be switched one at a time from their own telemetry
          settings.
        </AlertDescription>
      </Alert>
    )
  }

  if (isLoading) {
    return (
      <div className="space-y-3">
        <Skeleton className="h-4 w-64" />
        <Skeleton className="h-20 w-full" />
      </div>
    )
  }

  if (!job) {
    return (
      <p className="text-xs leading-5 text-muted-foreground">
        No activation is running. Starting one quotes every eligible project
        first — you see the exact span count and metered bytes before anything
        is sent.
      </p>
    )
  }

  return (
    <div className="space-y-4">
      <p className="text-xs leading-5 text-muted-foreground">
        {JOB_STATUS_DETAIL[job.status]}
      </p>

      {job.abort_detail && (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>This activation stopped early</AlertTitle>
          {/* The server's sentence names the fix and the page. Ours would not. */}
          <AlertDescription>{job.abort_detail}</AlertDescription>
        </Alert>
      )}

      {!job.abort_detail && job.abort_reason && (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>This activation stopped early</AlertTitle>
          <AlertDescription className="font-mono text-xs">
            {job.abort_reason}
          </AlertDescription>
        </Alert>
      )}

      <JobProgress job={job} projectLabel={projectLabel} />

      <JobProjectTable
        projects={job.projects}
        projectLabel={projectLabel}
        slugByProjectId={slugByProjectId}
      />

      <JobActions job={job} onCancel={onCancel} onRetryScoped={onRetryScoped} />
    </div>
  )
}

function JobActions({
  job,
  onCancel,
  onRetryScoped,
}: {
  job: BulkActivationJobResponse
  onCancel: () => void
  onRetryScoped: (projectIds: number[]) => void
}) {
  if (isJobActive(job)) {
    return (
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
        <Button
          variant="outline"
          size="sm"
          onClick={onCancel}
          disabled={job.cancel_requested}
        >
          {job.cancel_requested ? 'Stopping…' : 'Cancel activation'}
        </Button>
        <p className="text-xs text-muted-foreground">
          {job.cancel_requested
            ? 'A stop was requested and will be honoured at the next chunk boundary.'
            : 'Stopping is clean: the cursor is durable, so resuming re-sends nothing.'}
        </p>
      </div>
    )
  }

  // Only `completed_with_failures` gets a retry — the other terminal states
  // mean something different and must not offer the same button.
  if (job.status === 'completed_with_failures') {
    const ids = retryableProjectIds(job)
    return (
      <RetryAction
        label={`Retry ${ids.length} project${ids.length === 1 ? '' : 's'}`}
        description="Re-quotes only the projects that failed or were skipped, and asks you to confirm the new bill before anything is sent."
        projectIds={ids}
        onRetryScoped={onRetryScoped}
      />
    )
  }

  // `aborted` and `cancelled` resume rather than restart: the projects the job
  // never reached are still queued, and nothing already shipped is re-sent.
  if (job.status === 'aborted' || job.status === 'cancelled') {
    const ids = resumableProjectIds(job)
    return (
      <RetryAction
        label={`Resume ${ids.length} project${ids.length === 1 ? '' : 's'}`}
        description="Picks up the projects this activation never finished. Spans already acknowledged by Temps Cloud are not sent again."
        projectIds={ids}
        onRetryScoped={onRetryScoped}
      />
    )
  }

  return null
}

function RetryAction({
  label,
  description,
  projectIds,
  onRetryScoped,
}: {
  label: string
  description: string
  projectIds: number[]
  onRetryScoped: (projectIds: number[]) => void
}) {
  if (projectIds.length === 0) return null
  return (
    <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
      <Button
        variant="outline"
        size="sm"
        className="gap-1.5"
        onClick={() => onRetryScoped(projectIds)}
      >
        <RefreshCw className="size-3.5" />
        {label}
      </Button>
      <p className="text-xs text-muted-foreground">{description}</p>
    </div>
  )
}

/**
 * The entry point, in every state.
 *
 * On an unlinked instance this is a visible, disabled button plus the server's
 * reason and a link to Cloud setup — never nothing.
 */
function StartAction({
  configured,
  statusPending,
  reason,
  setupPath,
  jobIsActive,
  isQuoting,
  isDenied,
  onStart,
}: {
  configured: boolean
  statusPending: boolean
  reason?: string
  setupPath?: string
  jobIsActive: boolean
  isQuoting: boolean
  isDenied: boolean
  onStart: () => void
}) {
  const blocked = statusPending || !configured || jobIsActive || isDenied
  const resolvedSetupPath = isInternalConsolePath(setupPath)
    ? setupPath
    : CLOUD_SETUP_PATH

  return (
    <div className="space-y-2 border-t pt-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
        <Button
          size="sm"
          className="gap-1.5"
          onClick={onStart}
          disabled={blocked || isQuoting}
        >
          {isQuoting ? (
            <Loader2 className="size-3.5 animate-spin" />
          ) : (
            <CloudUpload className="size-3.5" />
          )}
          Switch all projects to Cloud
        </Button>
        {!blocked && (
          <p className="text-xs text-muted-foreground">
            Quotes every eligible project first. Nothing is sent until you
            confirm the total.
          </p>
        )}
      </div>

      <StartActionExplanation
        configured={configured}
        statusPending={statusPending}
        reason={reason}
        setupPath={resolvedSetupPath}
        jobIsActive={jobIsActive}
        isDenied={isDenied}
      />
    </div>
  )
}

function StartActionExplanation({
  configured,
  statusPending,
  reason,
  setupPath,
  jobIsActive,
  isDenied,
}: {
  configured: boolean
  statusPending: boolean
  reason?: string
  setupPath: string
  jobIsActive: boolean
  isDenied: boolean
}) {
  if (statusPending) {
    // Not "not connected" — not known yet. Saying the former for half a second
    // and then contradicting it is worse than saying nothing definite.
    return (
      <p className="text-xs leading-5 text-muted-foreground">
        Checking whether Temps Cloud is connected…
      </p>
    )
  }

  if (isDenied) {
    return (
      <p className="text-xs leading-5 text-muted-foreground">
        Starting an activation requires instance-administrator access.
      </p>
    )
  }

  if (jobIsActive) {
    return (
      <p className="text-xs leading-5 text-muted-foreground">
        An activation is already running. Only one runs at a time, so this stays
        disabled until it finishes or is cancelled.
      </p>
    )
  }

  if (configured) return null

  return (
    <div className="rounded-md border border-dashed bg-muted/40 p-3">
      <p className="text-xs font-medium">
        Temps Cloud is not set up for telemetry, so there is nothing to switch
        to yet
      </p>
      <p className="mt-1 text-xs leading-5 text-muted-foreground">
        {reason ??
          'This instance is not connected to Temps Cloud. Connect it to send spans to Cloud instead of storing them here.'}{' '}
        Once connected, this button quotes every project — for example &ldquo;12
        projects, 4.1 million spans, 3.2&nbsp;GB&rdquo; — and switches them all
        after you confirm.
      </p>
      <Button asChild size="sm" variant="outline" className="mt-3 gap-1.5">
        <Link to={setupPath}>
          Connect Temps Cloud
          <ArrowRight className="size-3.5" />
        </Link>
      </Button>
    </div>
  )
}
