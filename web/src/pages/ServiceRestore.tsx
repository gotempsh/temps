// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  cancelRestoreRunMutation,
  planRestoreMutation,
  startRestoreMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ExternalServiceInfo,
  RestorePlan,
  RestoreRunView,
  SourceBackupEntry,
} from '@/api/client/types.gen'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
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
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Checkbox } from '@/components/ui/checkbox'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { isPitrCapableFormat } from '@/lib/utils'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
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
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import {
  AlertCircle,
  AlertTriangle,
  ArrowLeft,
  ChevronLeft,
  ChevronRight,
  HardDrive,
  Loader2,
  RefreshCw,
  RotateCcw,
  Search,
  Star,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  Link,
  useLocation,
  useNavigate,
  useParams,
  useSearchParams,
} from 'react-router'
import { toast } from 'sonner'
import { RunTrackingPanel } from './service-restore/RunTrackingPanel'
import {
  restoreCapabilitiesQuery,
  restoreRunQuery,
  restoreServiceQuery,
  restoreSourceBackupsQuery,
  restoreSourcesQuery,
  serviceRestoreRunsQuery,
} from './service-restore/restore-queries'
import {
  activeRestoreConflict,
  attachReasonFromLocationState,
  classifyQueryError,
  completionToast,
  deriveRunTracking,
  markRunWatching,
  observeRun,
  parseRunParam,
  phaseLabel,
  pickActiveRun,
  restoreGate,
  runPollInterval,
  sectionLoadErrorCopy,
  sectionState,
  serviceLoadErrorCopy,
  sourcesState,
  type AttachReason,
  type CompletionLedger,
  type QueryErrorKind,
} from './service-restore/restore-state'
import {
  hasBackupSelection,
  isSelectedBackup,
  parseRestoreSelection,
  patchRestoreSelection,
  type RestoreSelectionPatch,
} from './service-restore/restore-selection'
import { cancelRefusal } from './service-restore/run-context'

type Mode = 'in_place' | 'new_service' | 'pitr'

// Engine-family check — mirror of the backend `engines_compatible` helper in
// crates/temps-backup/src/services/restore.rs. S3-compatible object stores
// all share the mc-mirror restore path and are mutually restorable.
const OBJECT_STORE_FAMILY = new Set(['s3', 'rustfs', 'minio', 'blob'])
function enginesCompatible(
  backupEngine: string | null | undefined,
  targetEngine: string
): boolean {
  const a = (backupEngine ?? '').toLowerCase()
  const b = targetEngine.toLowerCase()
  if (!a) return false
  if (a === b) return true
  return OBJECT_STORE_FAMILY.has(a) && OBJECT_STORE_FAMILY.has(b)
}

export function ServiceRestore() {
  const { id } = useParams<{ id: string }>()
  const serviceId = id ? Number(id) : NaN
  const validServiceId = Number.isFinite(serviceId)
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [searchParams, setSearchParams] = useSearchParams()
  const location = useLocation()
  const runParam = parseRunParam(searchParams.get('run'))
  usePageTitle('Restore service')
  const { setBreadcrumbs } = useBreadcrumbs()

  // ----- Queries ------------------------------------------------------------
  // Each read keeps its HTTP status on failure (see restore-queries.ts) so
  // the page can tell "no access" from "gone" from "API unavailable" instead
  // of spinning forever.
  const serviceQuery = useQuery({
    ...restoreServiceQuery(serviceId),
    enabled: validServiceId,
  })
  const service = serviceQuery.data?.service

  const capsQuery = useQuery({
    ...restoreCapabilitiesQuery(serviceId),
    enabled: validServiceId,
  })
  const capabilities = capsQuery.data
  const capsSection = sectionState(capsQuery)
  const capsReady = capsSection.kind === 'ready'

  const sourcesQuery = useQuery({
    ...restoreSourcesQuery(),
    enabled: validServiceId,
  })
  const s3Sources = sourcesQuery.data
  const sourcesView = sourcesState(sourcesQuery)
  const defaultSource = useMemo(
    () => s3Sources?.find((s) => s.is_default === true),
    [s3Sources]
  )

  // Without a run in the URL, ask the server whether this service already
  // has a restore in flight (a reload, another tab, another operator) and
  // follow it rather than offering to start a second one.
  const activeRunsQuery = useQuery({
    ...serviceRestoreRunsQuery(serviceId),
    enabled: validServiceId && runParam === null,
  })
  const activeRun =
    runParam === null &&
    activeRunsQuery.isSuccess &&
    !activeRunsQuery.isFetching
      ? pickActiveRun(activeRunsQuery.data)
      : undefined
  const activeRunsSection =
    runParam === null
      ? sectionState(activeRunsQuery)
      : ({ kind: 'ready' } as const)

  // ----- Run tracking ---------------------------------------------------------
  // The run being followed lives in the URL (`?run=<id>`), so a reload, a deep
  // link or signing in again resumes tracking it.
  const effectiveRunId = runParam ?? activeRun?.id ?? null
  const trackedRunId =
    typeof effectiveRunId === 'number' ? effectiveRunId : null
  // Runs this page has seen active, so completion is announced exactly once
  // per run, and never for a run that had already finished on arrival.
  const ledgerRef = useRef<CompletionLedger>({})

  const followRun = useCallback(
    (runId: number, reason?: AttachReason) => {
      ledgerRef.current = markRunWatching(ledgerRef.current, runId)
      setSearchParams(
        (prev) => {
          const next = new URLSearchParams(prev)
          next.set('run', String(runId))
          return next
        },
        {
          replace: true,
          state: reason ? { restoreAttach: reason } : null,
        }
      )
    },
    [setSearchParams]
  )

  const activeRunsUpdatedAt = activeRunsQuery.dataUpdatedAt
  useEffect(() => {
    if (!activeRun) return
    // The list row is a server-confirmed status: show it straight away, with
    // the time it was actually read, while the run's own status loads.
    const runKey = restoreRunQuery(activeRun.id).queryKey
    if (queryClient.getQueryData(runKey) === undefined) {
      queryClient.setQueryData(runKey, activeRun, {
        updatedAt: activeRunsUpdatedAt,
      })
    }
    followRun(activeRun.id, 'reattached')
  }, [activeRun, activeRunsUpdatedAt, followRun, queryClient])

  const runQuery = useQuery({
    ...restoreRunQuery(trackedRunId ?? 0),
    enabled: trackedRunId !== null,
    refetchInterval: (query) =>
      runPollInterval(
        query.state.data,
        query.state.error,
        query.state.status === 'error'
      ),
  })
  const runRow = runQuery.data

  useEffect(() => {
    if (!runRow) return
    const { ledger, notify } = observeRun(ledgerRef.current, runRow)
    ledgerRef.current = ledger
    if (!notify) return
    const message = completionToast(notify, runRow, service?.name)
    toast[message.level](message.title, { description: message.description })
  }, [runRow, service?.name])

  const runView =
    effectiveRunId === null
      ? null
      : deriveRunTracking(effectiveRunId, {
          isError: runQuery.isError,
          error: runQuery.error,
          data: runRow,
          dataUpdatedAt: runQuery.dataUpdatedAt,
        })
  const attachReason: AttachReason | undefined =
    runParam === null && activeRun
      ? 'reattached'
      : attachReasonFromLocationState(location.state)

  // ----- Selection ----------------------------------------------------------
  // The source, backup and mode live in the URL (`?source=&backup=&mode=`),
  // so a reload, a shared link or a "Restore this backup" link reproduces
  // them. See restore-selection.ts.
  const selection = parseRestoreSelection(searchParams)
  const selectedSourceId = selection.sourceId
  const mode: Mode = selection.mode
  const updateSelection = useCallback(
    (patch: RestoreSelectionPatch) =>
      setSearchParams((prev) => patchRestoreSelection(prev, patch), {
        replace: true,
      }),
    [setSearchParams]
  )

  // ----- Local state --------------------------------------------------------
  // Kept in this component, which stays mounted through load errors and
  // retries, so a retry never discards what the user already picked.
  // `undefined` means the suggestion has not been edited. An explicit empty
  // string must stay empty so validation can reject a cleared name.
  const [newServiceName, setNewServiceName] = useState<string | undefined>()
  const [pitrTargetTime, setPitrTargetTime] = useState('')
  const [pitrToNewService, setPitrToNewService] = useState(false)
  const [confirmText, setConfirmText] = useState('')
  const [search, setSearch] = useState('')
  const [showDestructiveConfirm, setShowDestructiveConfirm] = useState(false)
  const effectiveSourceId = selectedSourceId ?? defaultSource?.id
  const effectiveNewServiceName =
    newServiceName ?? capabilities?.suggested_new_service_name ?? ''
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  // Breadcrumbs: a safe label while the database is loading or unreadable.
  const serviceName = service?.name
  useEffect(() => {
    if (!validServiceId) return
    setBreadcrumbs([
      { label: 'Databases', href: '/storage' },
      {
        label: serviceName ?? `Database ${serviceId}`,
        href: `/storage/${serviceId}`,
      },
      { label: 'Restore' },
    ])
    return () => setBreadcrumbs([])
  }, [serviceName, serviceId, validServiceId, setBreadcrumbs])

  // ----- Backups list ------------------------------------------------------
  const backupsQuery = useQuery({
    ...restoreSourceBackupsQuery(effectiveSourceId ?? 0),
    enabled: effectiveSourceId !== undefined,
  })
  const backupIndex = backupsQuery.data
  const backupsSection = sectionState(backupsQuery)
  const refetchBackups = backupsQuery.refetch

  const allBackups = useMemo<SourceBackupEntry[]>(
    () => backupIndex?.backups ?? [],
    [backupIndex]
  )

  // The backup the URL selects, once this source's backups have loaded. A
  // backup of another engine is never selected, even by a crafted link.
  // `find` returns the list's own entry, so its identity is stable.
  const targetEngine = (service?.service_type ?? '').toLowerCase()
  const selectedBackup = allBackups.find(
    (b) =>
      isSelectedBackup(b, selection) &&
      enginesCompatible(b.engine, targetEngine)
  )
  const selectionMissing =
    hasBackupSelection(selection) &&
    backupsSection.kind === 'ready' &&
    !selectedBackup

  // Filter rule: the backup row's `engine` must be in the same engine family
  // as the target service. Today the only multi-engine family is the
  // S3-compatible object stores (s3/rustfs/minio/blob), which all use the
  // same mc-mirror restore path. Every other engine is its own family.
  // Missing/null engine is still an exclusion — control-plane backups have
  // no engine tag and shouldn't slip into a service restore picker.
  const filteredBackups = useMemo(() => {
    const engine = (service?.service_type ?? '').toLowerCase()
    const q = search.trim().toLowerCase()
    const rows = allBackups.filter((b) => {
      if (!engine) return false
      if (!enginesCompatible(b.engine, engine)) return false
      if (!q) return true
      return (
        (b.origin_service_name ?? '').toLowerCase().includes(q) ||
        (b.backup_id ?? '').toLowerCase().includes(q) ||
        (b.location ?? '').toLowerCase().includes(q)
      )
    })
    return [...rows].sort((a, b) => {
      const ta = a.created_at ? new Date(a.created_at).getTime() : 0
      const tb = b.created_at ? new Date(b.created_at).getTime() : 0
      return tb - ta
    })
  }, [allBackups, service?.service_type, search])

  const BACKUPS_PAGE_SIZE = 5
  const [backupsPage, setBackupsPage] = useState(1)
  const backupsTotalPages = Math.max(
    1,
    Math.ceil(filteredBackups.length / BACKUPS_PAGE_SIZE)
  )

  if (backupsPage > backupsTotalPages) setBackupsPage(backupsTotalPages)

  // Open the page of the list that holds a backup selected by the URL, once.
  const pagedToSelectionRef = useRef<string | null>(null)
  useEffect(() => {
    if (!selectedBackup) return
    const key = `${selectedBackup.source}-${selectedBackup.id}-${selectedBackup.location}`
    if (pagedToSelectionRef.current === key) return
    pagedToSelectionRef.current = key
    const index = filteredBackups.indexOf(selectedBackup)
    if (index >= 0) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setBackupsPage(Math.floor(index / BACKUPS_PAGE_SIZE) + 1)
    }
  }, [selectedBackup, filteredBackups])

  const paginatedBackups = useMemo(
    () =>
      filteredBackups.slice(
        (backupsPage - 1) * BACKUPS_PAGE_SIZE,
        backupsPage * BACKUPS_PAGE_SIZE
      ),
    [filteredBackups, backupsPage]
  )

  const backupsPageWindow = useMemo(() => {
    const windowSize = Math.min(5, backupsTotalPages)
    const start = Math.max(
      1,
      Math.min(
        backupsPage - Math.floor(windowSize / 2),
        backupsTotalPages - windowSize + 1
      )
    )
    return Array.from({ length: windowSize }, (_, idx) => start + idx)
  }, [backupsPage, backupsTotalPages])

  // Incompat backups (dropped above) — count for context so user isn't confused
  const incompatCount = useMemo(() => {
    const engine = (service?.service_type ?? '').toLowerCase()
    if (!engine) return 0
    return allBackups.filter((b) => !enginesCompatible(b.engine, engine)).length
  }, [allBackups, service?.service_type])

  // ----- Mutations ---------------------------------------------------------
  // Starting a restore is never retried automatically: a failed status read
  // or a lost response is not evidence that the restore did not start.
  const startMutation = useMutation({
    ...startRestoreMutation(),
    retry: false,
    meta: { errorTitle: 'Failed to start restore' },
    onSuccess: (run) => {
      const r = run as RestoreRunView
      // The 202 body is the server's first confirmed status for this run.
      queryClient.setQueryData(restoreRunQuery(r.id).queryKey, r)
      setShowDestructiveConfirm(false)
      followRun(r.id)
      toast.success('Restore started', {
        description: `Run ${r.id} (phase: ${phaseLabel(r.phase)}).`,
      })
    },
    onError: (error, variables) => {
      if (
        handleSensitiveActionError(error, () => startMutation.mutate(variables))
      ) {
        setShowDestructiveConfirm(false)
        return
      }
      // 409 restore-already-active: follow the restore that is running
      // instead of reporting a failure.
      const activeRunId = activeRestoreConflict(error)
      if (activeRunId !== undefined) {
        setShowDestructiveConfirm(false)
        if (activeRunId !== null) {
          followRun(activeRunId, 'already_active')
        } else {
          void activeRunsQuery.refetch()
        }
        toast.info('A restore is already running on this database', {
          description:
            activeRunId !== null
              ? `Following run #${activeRunId} instead of starting a new one.`
              : 'Looking up the running restore instead of starting a new one.',
        })
        return
      }
      const problem = error as { detail?: string; message?: string }
      toast.error('Failed to start restore', {
        description: problem.detail || problem.message || 'Unknown error',
      })
    },
  })

  // Cancelling is refused by the server once the run starts writing data;
  // the run's own status (and its completion toast) report a success.
  const cancelMutation = useMutation({
    ...cancelRestoreRunMutation(),
    retry: false,
    onSuccess: (run) => {
      queryClient.setQueryData(restoreRunQuery(run.id).queryKey, run)
      void queryClient.invalidateQueries({
        queryKey: serviceRestoreRunsQuery(serviceId).queryKey,
      })
    },
    onError: (error) => {
      const copy = cancelRefusal(error)
      toast[copy.level](copy.title, { description: copy.description })
      void runQuery.refetch()
    },
  })

  const planMutation = useMutation({
    ...planRestoreMutation(),
    meta: { errorTitle: 'Failed to plan restore' },
  })

  const isOrphan = selectedBackup?.source === 's3_scan'
  const selectedSupportsPitr = isPitrCapableFormat(selectedBackup?.format)
  const isCrossService =
    !!selectedBackup?.origin_service_name &&
    !!service?.name &&
    selectedBackup.origin_service_name !== service.name

  const needsTypedConfirm =
    mode === 'in_place' || (mode === 'pitr' && !pitrToNewService)
  const confirmOk =
    !needsTypedConfirm || confirmText.trim() === (service?.name ?? '')

  // Build the plan/start request body. Shared by both the plan preview and
  // the actual start call so they stay in sync — if one says "this is safe"
  // the other submits the exact same thing.
  const buildRequestBody = (): Record<string, unknown> | null => {
    if (!selectedBackup) return null
    const base: Record<string, unknown> = isOrphan
      ? {
          backup_location: selectedBackup.location,
          backup_engine: selectedBackup.engine,
          s3_source_id: effectiveSourceId,
        }
      : { backup_id: selectedBackup.id }
    if (mode === 'in_place') return { ...base, mode: 'in_place' }
    if (mode === 'new_service')
      return {
        ...base,
        mode: 'new_service',
        name: effectiveNewServiceName.trim(),
        parameter_overrides: {},
      }
    // pitr
    if (!pitrTargetTime || Number.isNaN(new Date(pitrTargetTime).getTime()))
      return null
    return {
      ...base,
      mode: 'pitr',
      to_new_service: pitrToNewService,
      new_service_name: pitrToNewService
        ? effectiveNewServiceName.trim()
        : undefined,
      target: { kind: 'time', time: new Date(pitrTargetTime).toISOString() },
    }
  }

  const planRequestKey = JSON.stringify({
    id: serviceId,
    body: buildRequestBody(),
  })
  const [plannedRestore, setPlannedRestore] = useState<{
    key: string
    plan: RestorePlan
  } | null>(null)

  // Ignore responses for inputs the user has already changed.
  useEffect(() => {
    const body = buildRequestBody()
    if (!body) return
    let active = true
    planMutation.mutate(
      {
        path: { id: serviceId },
        body: body as never,
      },
      {
        onSuccess: (data) => {
          if (active)
            setPlannedRestore({
              key: planRequestKey,
              plan: data as RestorePlan,
            })
        },
      }
    )
    return () => {
      active = false
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    selectedBackup?.id,
    selectedBackup?.location,
    effectiveSourceId,
    mode,
    effectiveNewServiceName,
    pitrTargetTime,
    pitrToNewService,
    serviceId,
  ])

  const plan =
    plannedRestore?.key === planRequestKey ? plannedRestore.plan : undefined
  const planError = planMutation.error as Error | null
  const planHasBlockingErrors = !!plan && plan.errors.length > 0
  // The server decides cross-service by service identity (the backup's
  // recorded producer ids). Only a completed plan for these exact inputs
  // can authorize the confirmation and start action.
  const crossServiceRestore = plan?.cross_service ?? false

  // Capabilities and the active-run check must both be known before a
  // restore can be started.
  const gate = restoreGate(capsSection, activeRunsSection)

  const canSubmit = (() => {
    if (!gate.enabled) return false
    if (
      !selectedBackup ||
      !plan ||
      planMutation.isPending ||
      planMutation.isError
    )
      return false
    if (mode === 'new_service' && effectiveNewServiceName.trim().length === 0)
      return false
    if (mode === 'pitr') {
      if (!pitrTargetTime || Number.isNaN(new Date(pitrTargetTime).getTime()))
        return false
      if (pitrToNewService && effectiveNewServiceName.trim().length === 0)
        return false
      if (!selectedSupportsPitr) return false
    }
    if (!confirmOk) return false
    // Block on plan errors — user must resolve them (e.g. pick a different
    // backup or target) before we'll even let them click Start.
    if (planHasBlockingErrors) return false
    return true
  })()

  const doStart = () => {
    if (!selectedBackup || !canSubmit) return
    const base: Record<string, unknown> = isOrphan
      ? {
          backup_location: selectedBackup.location,
          backup_engine: selectedBackup.engine,
          s3_source_id: effectiveSourceId,
        }
      : { backup_id: selectedBackup.id }

    // Destructive restores only reach doStart() through the confirmation
    // dialog, which names the cross-service overwrite explicitly; send that
    // confirmation so the server can bind and audit it.
    if (needsTypedConfirm && crossServiceRestore) {
      base.confirm_cross_service = true
    }

    let body: Record<string, unknown>
    if (mode === 'in_place') {
      body = { ...base, mode: 'in_place' }
    } else if (mode === 'new_service') {
      body = {
        ...base,
        mode: 'new_service',
        name: effectiveNewServiceName.trim(),
        parameter_overrides: {},
      }
    } else {
      body = {
        ...base,
        mode: 'pitr',
        to_new_service: pitrToNewService,
        new_service_name: pitrToNewService
          ? effectiveNewServiceName.trim()
          : undefined,
        target: { kind: 'time', time: new Date(pitrTargetTime).toISOString() },
      }
    }

    startMutation.mutate({
      path: { id: serviceId },
      body: body as never,
    })
  }

  const handleStart = () => {
    // Destructive modes (in-place or PITR into the same service) require an
    // explicit confirmation dialog before the request is sent.
    if (needsTypedConfirm) {
      setShowDestructiveConfirm(true)
    } else {
      doStart()
    }
  }

  const startNewRestore = () => {
    // Forget the cached history so the active-run check reads the server
    // again rather than reattaching from a list fetched before this run ended.
    void queryClient.resetQueries({
      queryKey: serviceRestoreRunsQuery(serviceId).queryKey,
    })
    setConfirmText('')
    setSearchParams((prev) => {
      const next = new URLSearchParams(prev)
      next.delete('run')
      return next
    })
  }

  // ---------- Render --------------------------------------------------------

  if (!validServiceId) {
    return (
      <PageContainer>
        <Alert variant="destructive">
          <AlertCircle className="h-4 w-4" />
          <AlertDescription>Invalid service id.</AlertDescription>
        </Alert>
      </PageContainer>
    )
  }

  const serviceSection = sectionState(serviceQuery)

  if (!service) {
    return (
      <PageContainer>
        <RestorePageHeader />
        {serviceSection.kind === 'error' ? (
          <ServiceLoadError
            kind={serviceSection.errorKind}
            retrying={serviceQuery.isFetching}
            onRetry={() => void serviceQuery.refetch()}
            onBack={() => navigate('/storage')}
          />
        ) : (
          <RestorePageSkeleton />
        )}
      </PageContainer>
    )
  }

  // Running view: takes over the page while a run is followed. It never
  // offers the restore form's Start action; only a terminal run (or an id
  // that does not exist) leads back to the form.
  if (effectiveRunId !== null && runView) {
    return (
      <PageContainer>
        <RestorePageHeader service={service} />
        <RunTrackingPanel
          serviceId={serviceId}
          runId={effectiveRunId}
          view={runView}
          attachReason={attachReason}
          onRetryStatus={() => void runQuery.refetch()}
          retrying={runQuery.isFetching}
          onBack={() => navigate(`/storage/${serviceId}`)}
          onStartNew={startNewRestore}
          onOpenRestored={(targetId) => navigate(`/storage/${targetId}`)}
          onCancel={() => {
            if (typeof effectiveRunId === 'number')
              cancelMutation.mutate({ path: { id: effectiveRunId } })
          }}
          cancelling={cancelMutation.isPending}
        />
      </PageContainer>
    )
  }

  // Configure view. Modes stay disabled until the server has said which
  // ones this database supports.
  const inPlaceDisabled = !capsReady || capabilities?.restore_in_place === false
  const newServiceDisabled =
    !capsReady || capabilities?.restore_to_new_service === false
  const pitrDisabled =
    !capsReady || capabilities?.pitr === false || !selectedSupportsPitr
  return (
    <PageContainer>
      <RestorePageHeader service={service} />

      {/* Step 1: S3 source */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-base">
            <span className="inline-flex h-6 w-6 items-center justify-center rounded-full bg-primary text-primary-foreground text-xs font-semibold">
              1
            </span>
            Storage source
          </CardTitle>
          <CardDescription>
            Pick the S3-compatible source that holds the backup. Backups from
            previous Temps instances appear here too.
          </CardDescription>
        </CardHeader>
        <CardContent>
          {sourcesView.kind === 'loading' ? (
            <Skeleton
              className="h-10 w-full max-w-md"
              aria-label="Loading storage sources"
            />
          ) : sourcesView.kind === 'error' ? (
            <InlineLoadError
              title="Could not load storage sources"
              description={sectionLoadErrorCopy(
                'the storage sources',
                sourcesView.errorKind
              )}
              retrying={sourcesQuery.isFetching}
              onRetry={() => void sourcesQuery.refetch()}
            />
          ) : sourcesView.kind === 'empty' ? (
            <EmptyState
              size="compact"
              icon={HardDrive}
              title="No storage sources yet"
              description="Backups are read from an S3-compatible storage source. Add one to list the backups you can restore from."
              action={
                <Button variant="outline" asChild>
                  <Link to="/backups/s3-sources/new">Add storage source</Link>
                </Button>
              }
            />
          ) : (
            <Select
              value={effectiveSourceId?.toString()}
              onValueChange={(v) => {
                updateSelection({ sourceId: Number(v), backup: null })
                setBackupsPage(1)
              }}
            >
              <SelectTrigger className="max-w-md">
                <SelectValue placeholder="Select an S3 source" />
              </SelectTrigger>
              <SelectContent>
                {s3Sources?.map((source) => {
                  const isDefault = source.is_default === true
                  return (
                    <SelectItem key={source.id} value={source.id.toString()}>
                      <span className="flex items-center gap-2">
                        {source.name}
                        {isDefault ? (
                          <Star className="h-3 w-3 fill-amber-500 text-amber-500" />
                        ) : null}
                        <span className="text-xs text-muted-foreground">
                          {source.bucket_name}
                        </span>
                      </span>
                    </SelectItem>
                  )
                })}
              </SelectContent>
            </Select>
          )}
        </CardContent>
      </Card>

      {/* Step 2: pick a backup */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-base">
            <span className="inline-flex h-6 w-6 items-center justify-center rounded-full bg-primary text-primary-foreground text-xs font-semibold">
              2
            </span>
            Pick a backup
          </CardTitle>
          <CardDescription>
            All {service.service_type} backups on this source. Backups produced
            by a different service can be selected — useful for
            disaster-recovery restores.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <div className="flex items-center gap-2">
            <div className="relative flex-1 max-w-md">
              <Search className="h-3.5 w-3.5 absolute left-2.5 top-1/2 -translate-y-1/2 text-muted-foreground" />
              <Input
                className="pl-8"
                placeholder="Filter by origin service, UUID, or path…"
                value={search}
                onChange={(e) => {
                  setSearch(e.target.value)
                  setBackupsPage(1)
                }}
              />
            </div>
            <Button
              variant="outline"
              size="sm"
              onClick={() => refetchBackups()}
              disabled={
                effectiveSourceId === undefined || backupsQuery.isFetching
              }
            >
              Refresh
            </Button>
            {incompatCount > 0 ? (
              <span className="text-xs text-muted-foreground">
                {incompatCount} backup{incompatCount === 1 ? '' : 's'} hidden
                (wrong engine)
              </span>
            ) : null}
          </div>

          {backupsQuery.isError && backupIndex !== undefined ? (
            <InlineLoadError
              title="Could not refresh backups"
              description={`Showing the backups loaded earlier. ${sectionLoadErrorCopy('the backups on this source', classifyQueryError(backupsQuery.error))}`}
              retrying={backupsQuery.isFetching}
              onRetry={() => void refetchBackups()}
            />
          ) : null}

          {effectiveSourceId === undefined ? (
            <div className="text-sm text-muted-foreground py-8 text-center border rounded-md">
              {sourcesView.kind === 'ready'
                ? 'Select a storage source to list its backups.'
                : sourcesView.kind === 'empty'
                  ? 'Add a storage source to list its backups.'
                  : 'Backups are listed once a storage source is available.'}
            </div>
          ) : backupsSection.kind === 'loading' ? (
            <BackupsSkeleton />
          ) : backupsSection.kind === 'error' ? (
            <InlineLoadError
              title="Could not load backups from this source"
              description={
                backupsSection.errorKind === 'unavailable'
                  ? 'Could not load the backups on this source. Check your connection, and that the S3 endpoint and bucket path are correct, then retry.'
                  : sectionLoadErrorCopy(
                      'the backups on this source',
                      backupsSection.errorKind
                    )
              }
              retrying={backupsQuery.isFetching}
              onRetry={() => void refetchBackups()}
            />
          ) : filteredBackups.length === 0 ? (
            <div className="text-sm text-muted-foreground py-8 text-center border rounded-md">
              No compatible backups on this source.
            </div>
          ) : (
            <div className="border rounded-md">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-8"></TableHead>
                    <TableHead>Created</TableHead>
                    <TableHead>Origin service</TableHead>
                    <TableHead>Format</TableHead>
                    <TableHead>Size</TableHead>
                    <TableHead>Source</TableHead>
                    <TableHead>State</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {paginatedBackups.map((b) => {
                    const isSel =
                      selectedBackup &&
                      selectedBackup.location === b.location &&
                      selectedBackup.id === b.id
                    return (
                      <TableRow
                        key={`${b.source}-${b.id}-${b.location}`}
                        className={`cursor-pointer ${isSel ? 'bg-accent' : ''}`}
                        onClick={() => updateSelection({ backup: b })}
                      >
                        <TableCell>
                          <RadioGroup value={isSel ? 'on' : ''}>
                            <RadioGroupItem value="on" checked={!!isSel} />
                          </RadioGroup>
                        </TableCell>
                        <TableCell className="font-mono text-xs whitespace-nowrap">
                          {b.created_at
                            ? new Date(b.created_at).toLocaleString()
                            : '—'}
                        </TableCell>
                        <TableCell>{b.origin_service_name ?? '—'}</TableCell>
                        <TableCell>
                          <FormatBadge format={b.format} />
                        </TableCell>
                        <TableCell className="text-xs text-muted-foreground">
                          {b.size_bytes ? formatBytes(b.size_bytes) : '—'}
                        </TableCell>
                        <TableCell>
                          <Badge variant="outline" className="text-xs">
                            {b.source === 'db' ? 'Tracked' : 'S3 only'}
                          </Badge>
                        </TableCell>
                        <TableCell>
                          <Badge
                            variant={
                              b.state === 'completed' ? 'default' : 'secondary'
                            }
                            className="text-xs"
                          >
                            {b.state || '—'}
                          </Badge>
                        </TableCell>
                      </TableRow>
                    )
                  })}
                </TableBody>
              </Table>
            </div>
          )}

          {effectiveSourceId !== undefined &&
            backupsSection.kind === 'ready' &&
            backupsTotalPages > 1 && (
              <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
                <div className="text-sm text-muted-foreground">
                  <span className="hidden sm:inline tabular-nums">
                    Showing {(backupsPage - 1) * BACKUPS_PAGE_SIZE + 1} to{' '}
                    {Math.min(
                      backupsPage * BACKUPS_PAGE_SIZE,
                      filteredBackups.length
                    )}{' '}
                    of {filteredBackups.length} backups
                  </span>
                  <span className="sm:hidden tabular-nums">
                    {backupsPage} / {backupsTotalPages}
                  </span>
                </div>
                <div className="flex items-center gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setBackupsPage((p) => Math.max(1, p - 1))}
                    disabled={backupsPage === 1}
                  >
                    <ChevronLeft className="h-4 w-4" />
                    <span className="hidden sm:inline">Previous</span>
                  </Button>
                  <div className="hidden sm:flex items-center gap-1">
                    {backupsPageWindow.map((pageNum) => (
                      <Button
                        key={pageNum}
                        variant={
                          pageNum === backupsPage ? 'default' : 'outline'
                        }
                        size="sm"
                        onClick={() => setBackupsPage(pageNum)}
                        className="w-10"
                      >
                        {pageNum}
                      </Button>
                    ))}
                  </div>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() =>
                      setBackupsPage((p) => Math.min(backupsTotalPages, p + 1))
                    }
                    disabled={backupsPage === backupsTotalPages}
                  >
                    <span className="hidden sm:inline">Next</span>
                    <ChevronRight className="h-4 w-4" />
                  </Button>
                </div>
              </div>
            )}

          {selectionMissing ? (
            <Alert variant="warning">
              <AlertTriangle className="h-4 w-4" />
              <AlertTitle>The linked backup is not on this source</AlertTitle>
              <AlertDescription>
                This page was opened for a backup that this storage source does
                not list for a {service.service_type} database. It may have been
                deleted, or it is stored on another source. Pick a backup below.
              </AlertDescription>
            </Alert>
          ) : null}

          {selectedBackup ? (
            <div className="text-xs text-muted-foreground font-mono break-all pt-2">
              Selected: {selectedBackup.location || '(no location)'}
            </div>
          ) : null}

          {isCrossService ? (
            <Alert>
              <AlertTriangle className="h-4 w-4" />
              <AlertDescription>
                This backup was produced by{' '}
                <strong>{selectedBackup?.origin_service_name}</strong>, not{' '}
                <strong>{service.name}</strong>. Make sure you intend to restore
                foreign data onto this service.
              </AlertDescription>
            </Alert>
          ) : null}
        </CardContent>
      </Card>

      {/* Step 3: mode */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-base">
            <span className="inline-flex h-6 w-6 items-center justify-center rounded-full bg-primary text-primary-foreground text-xs font-semibold">
              3
            </span>
            Restore mode
          </CardTitle>
          <CardDescription>
            Where should the restored data land?
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          {capsSection.kind === 'loading' ? (
            <p className="text-sm text-muted-foreground">
              Checking which restore modes this database supports…
            </p>
          ) : capsSection.kind === 'error' ? (
            <InlineLoadError
              title="Could not load the restore options"
              description={`${sectionLoadErrorCopy('the restore options for this database', capsSection.errorKind)} Restore modes stay disabled until they load.`}
              retrying={capsQuery.isFetching}
              onRetry={() => void capsQuery.refetch()}
            />
          ) : null}
          <RadioGroup
            value={mode}
            onValueChange={(v) => updateSelection({ mode: v as Mode })}
            className="grid gap-3"
          >
            <label
              htmlFor="mode-in-place"
              className={`flex items-start gap-3 rounded-md border p-3 cursor-pointer ${
                mode === 'in_place' ? 'border-primary bg-accent/50' : ''
              } ${inPlaceDisabled ? 'opacity-50 cursor-not-allowed' : ''}`}
            >
              <RadioGroupItem
                value="in_place"
                id="mode-in-place"
                disabled={inPlaceDisabled}
                className="mt-0.5"
              />
              <div className="flex-1">
                <div className="font-medium flex items-center gap-2">
                  In-place restore
                  <Badge variant="destructive" className="text-xs">
                    Destructive
                  </Badge>
                </div>
                <div className="text-xs text-muted-foreground mt-0.5">
                  Overwrites current data on <strong>{service.name}</strong>.
                </div>
              </div>
            </label>

            <label
              htmlFor="mode-new"
              className={`flex items-start gap-3 rounded-md border p-3 cursor-pointer ${
                mode === 'new_service' ? 'border-primary bg-accent/50' : ''
              } ${newServiceDisabled ? 'opacity-50 cursor-not-allowed' : ''}`}
            >
              <RadioGroupItem
                value="new_service"
                id="mode-new"
                disabled={newServiceDisabled}
                className="mt-0.5"
              />
              <div className="flex-1">
                <div className="font-medium">Clone into a new service</div>
                <div className="text-xs text-muted-foreground mt-0.5">
                  Provisions a sibling of <strong>{service.name}</strong> with
                  the restored data. Original is untouched.
                </div>
              </div>
            </label>

            <label
              htmlFor="mode-pitr"
              className={`flex items-start gap-3 rounded-md border p-3 cursor-pointer ${
                mode === 'pitr' ? 'border-primary bg-accent/50' : ''
              } ${pitrDisabled ? 'opacity-50 cursor-not-allowed' : ''}`}
            >
              <RadioGroupItem
                value="pitr"
                id="mode-pitr"
                disabled={pitrDisabled}
                className="mt-0.5"
              />
              <div className="flex-1">
                <div className="font-medium">Point-in-time recovery</div>
                <div className="text-xs text-muted-foreground mt-0.5">
                  {service?.service_type === 'mariadb'
                    ? 'Recover to a specific timestamp by replaying archived binlogs. Requires a physical (mariadb-backup) base backup.'
                    : 'Recover to a specific timestamp via WAL replay. Requires a WAL-G backup.'}
                  {selectedBackup && !selectedSupportsPitr
                    ? selectedBackup.format === 'mariadb_dump'
                      ? ' Selected backup is a logical dump; PITR not available.'
                      : ' Selected backup is pg_dump; PITR not available.'
                    : ''}
                </div>
              </div>
            </label>
          </RadioGroup>

          {mode === 'new_service' ? (
            <div className="space-y-2 pt-2">
              <Label htmlFor="new-service-name">New service name</Label>
              <Input
                id="new-service-name"
                value={effectiveNewServiceName}
                onChange={(e) => setNewServiceName(e.target.value)}
                className="max-w-md"
              />
              <p className="text-xs text-muted-foreground">
                Suggested: {capabilities?.suggested_new_service_name}. The new
                service uses the same image and credentials as{' '}
                <strong>{service.name}</strong>.
              </p>
            </div>
          ) : null}

          {mode === 'pitr' ? (
            <div className="space-y-3 pt-2">
              <div className="space-y-2">
                <Label htmlFor="pitr-time">Target time (UTC)</Label>
                <Input
                  id="pitr-time"
                  type="datetime-local"
                  value={pitrTargetTime}
                  onChange={(e) => setPitrTargetTime(e.target.value)}
                  className="max-w-md"
                />
                <p className="text-xs text-muted-foreground">
                  {service?.service_type === 'mariadb'
                    ? 'MariaDB will replay archived binlogs from the base backup through this time.'
                    : 'PostgreSQL will replay archived WAL from the base backup through this time.'}
                </p>
              </div>
              <div className="flex items-center gap-2">
                <Checkbox
                  id="pitr-new"
                  checked={pitrToNewService}
                  onCheckedChange={(checked) =>
                    setPitrToNewService(checked === true)
                  }
                />
                <Label htmlFor="pitr-new" className="cursor-pointer">
                  Restore into a new service (leaves {service.name} untouched)
                </Label>
              </div>
              {pitrToNewService ? (
                <div className="space-y-2">
                  <Label htmlFor="pitr-new-name">New service name</Label>
                  <Input
                    id="pitr-new-name"
                    value={effectiveNewServiceName}
                    onChange={(e) => setNewServiceName(e.target.value)}
                    className="max-w-md"
                  />
                </div>
              ) : null}
            </div>
          ) : null}

          {needsTypedConfirm ? (
            <Alert variant="destructive">
              <AlertTriangle className="h-4 w-4" />
              <AlertDescription>
                This will <strong>OVERWRITE</strong> data on{' '}
                <strong>{service.name}</strong>. The service will be briefly
                unavailable. Type the service name to confirm.
              </AlertDescription>
            </Alert>
          ) : null}
          {needsTypedConfirm ? (
            <div className="space-y-2">
              <Label htmlFor="confirm-name">
                Type <code>{service.name}</code> to confirm
              </Label>
              <Input
                id="confirm-name"
                value={confirmText}
                onChange={(e) => setConfirmText(e.target.value)}
                placeholder={service.name}
                className="max-w-md"
              />
            </div>
          ) : null}
        </CardContent>
      </Card>

      {/* Step 4: plan preview — mounted when user has picked a backup. */}
      {selectedBackup ? (
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-base">
              <span className="inline-flex h-6 w-6 items-center justify-center rounded-full bg-primary text-primary-foreground text-xs font-semibold">
                4
              </span>
              Preview what will happen
            </CardTitle>
            <CardDescription>
              Exact sequence of actions the restore orchestrator will run.
              Review before confirming.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-3">
            {planMutation.isPending && !plan ? (
              <div className="flex items-center text-sm text-muted-foreground py-4">
                <Loader2 className="h-4 w-4 animate-spin mr-2" />
                Computing plan…
              </div>
            ) : planError ? (
              <Alert variant="destructive">
                <AlertCircle className="h-4 w-4" />
                <AlertDescription>
                  Failed to compute plan: {planError.message}
                </AlertDescription>
              </Alert>
            ) : plan ? (
              <>
                <div className="flex flex-wrap items-center gap-2 text-xs">
                  <Badge variant="outline" className="font-mono">
                    {plan.strategy}
                  </Badge>
                  {plan.destructive ? (
                    <Badge variant="destructive">destructive</Badge>
                  ) : (
                    <Badge variant="secondary">non-destructive</Badge>
                  )}
                  <span className="text-muted-foreground">
                    target: <code>{plan.target_service.container}</code>
                  </span>
                </div>

                {plan.errors.length > 0 ? (
                  <Alert variant="destructive">
                    <AlertCircle className="h-4 w-4" />
                    <AlertDescription>
                      <strong>Cannot proceed:</strong>
                      <ul className="list-disc pl-5 mt-1 space-y-0.5">
                        {plan.errors.map((e) => (
                          <li key={e}>{e}</li>
                        ))}
                      </ul>
                    </AlertDescription>
                  </Alert>
                ) : null}

                {plan.warnings.length > 0 ? (
                  <Alert>
                    <AlertTriangle className="h-4 w-4" />
                    <AlertDescription>
                      <ul className="list-disc pl-5 space-y-0.5">
                        {plan.warnings.map((w) => (
                          <li key={w}>{w}</li>
                        ))}
                      </ul>
                    </AlertDescription>
                  </Alert>
                ) : null}

                {plan.source_backup.location_was_resolved ? (
                  <p className="text-xs text-muted-foreground">
                    Will use resolved location:{' '}
                    <code className="break-all">
                      {plan.source_backup.location}
                    </code>
                  </p>
                ) : null}

                <div>
                  <div className="text-sm font-medium mb-2">Steps</div>
                  <ol className="space-y-1.5 list-decimal pl-5 text-sm">
                    {plan.steps.map((s, i) => (
                      <li key={i} className="text-foreground/90">
                        {s}
                      </li>
                    ))}
                  </ol>
                </div>
              </>
            ) : null}
          </CardContent>
        </Card>
      ) : null}

      {/* Action bar */}
      {!gate.enabled && gate.reason ? (
        gate.retry === 'active_runs' ? (
          <InlineLoadError
            title="Could not check for a running restore"
            description={gate.reason}
            retrying={activeRunsQuery.isFetching}
            onRetry={() => void activeRunsQuery.refetch()}
          />
        ) : (
          <p className="text-sm text-muted-foreground">{gate.reason}</p>
        )
      ) : null}
      <div className="flex justify-between items-center gap-3 pt-2">
        <Button
          variant="outline"
          onClick={() => navigate(`/storage/${serviceId}`)}
        >
          <ArrowLeft className="h-4 w-4 mr-2" />
          Cancel
        </Button>
        <Button
          onClick={handleStart}
          disabled={!canSubmit || startMutation.isPending}
          size="lg"
        >
          {startMutation.isPending ? (
            <Loader2 className="h-4 w-4 mr-2 animate-spin" />
          ) : (
            <RotateCcw className="h-4 w-4 mr-2" />
          )}
          Start restore
        </Button>
      </div>

      {/* Destructive-restore confirmation dialog */}
      <AlertDialog
        open={showDestructiveConfirm}
        onOpenChange={(open: boolean) => {
          if (!startMutation.isPending) setShowDestructiveConfirm(open)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Overwrite live database?</AlertDialogTitle>
            <AlertDialogDescription>
              This restore will stop{' '}
              <strong>{service?.name ?? 'the service'}</strong> and replace its
              entire dataset with the selected backup. All data written since
              the backup was taken will be permanently lost. This action cannot
              be undone.
              {crossServiceRestore ? (
                <>
                  {' '}
                  The backup was produced by{' '}
                  <strong>
                    {selectedBackup?.origin_service_name ?? 'another service'}
                  </strong>
                  , not this service: confirming records an explicit
                  cross-service restore in the audit log.
                </>
              ) : null}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={startMutation.isPending}>
              Go back
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={(e: { preventDefault: () => void }) => {
                e.preventDefault()
                doStart()
              }}
              disabled={startMutation.isPending}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {startMutation.isPending
                ? 'Starting restore…'
                : 'Yes, overwrite database'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {verificationDialog}
    </PageContainer>
  )
}

// ----- Smaller pieces ------------------------------------------------------

function RestorePageHeader({
  service,
}: {
  service?: Pick<ExternalServiceInfo, 'name' | 'service_type'>
}) {
  return (
    <PageHeader
      title="Restore service"
      description={
        service ? (
          <>
            Target: <strong>{service.name}</strong> ({service.service_type})
          </>
        ) : undefined
      }
    />
  )
}

/** A read that failed: what failed, why, and a Retry that repeats it. */
function InlineLoadError({
  title,
  description,
  retrying,
  onRetry,
}: {
  title: string
  description: string
  retrying: boolean
  onRetry: () => void
}) {
  return (
    <Alert variant="destructive">
      <AlertCircle className="h-4 w-4" />
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription className="space-y-3">
        <p>{description}</p>
        <Button
          variant="outline"
          size="sm"
          onClick={onRetry}
          disabled={retrying}
        >
          {retrying ? (
            <Loader2 className="h-4 w-4 mr-2 animate-spin" />
          ) : (
            <RefreshCw className="h-4 w-4 mr-2" />
          )}
          Retry
        </Button>
      </AlertDescription>
    </Alert>
  )
}

function ServiceLoadError({
  kind,
  retrying,
  onRetry,
  onBack,
}: {
  kind: QueryErrorKind
  retrying: boolean
  onRetry: () => void
  onBack: () => void
}) {
  const copy = serviceLoadErrorCopy(kind)
  return (
    <div className="space-y-4">
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertTitle>{copy.title}</AlertTitle>
        <AlertDescription>{copy.description}</AlertDescription>
      </Alert>
      <div className="flex flex-wrap gap-2">
        <Button onClick={onRetry} disabled={retrying}>
          {retrying ? (
            <Loader2 className="h-4 w-4 mr-2 animate-spin" />
          ) : (
            <RefreshCw className="h-4 w-4 mr-2" />
          )}
          Retry
        </Button>
        <Button variant="outline" onClick={onBack}>
          <ArrowLeft className="h-4 w-4 mr-2" />
          Back to databases
        </Button>
      </div>
    </div>
  )
}

/** Placeholder shaped like the restore steps while the database loads. */
function RestorePageSkeleton() {
  return (
    <div className="space-y-6" aria-label="Loading restore options">
      {[0, 1, 2].map((step) => (
        <div key={step} className="space-y-3 rounded-lg border p-6">
          <div className="flex items-center gap-2">
            <Skeleton className="h-6 w-6 rounded-full" />
            <Skeleton className="h-5 w-40" />
          </div>
          <Skeleton className="h-4 w-full max-w-lg" />
          <Skeleton className="h-10 w-full max-w-md" />
        </div>
      ))}
    </div>
  )
}

function BackupsSkeleton() {
  return (
    <div
      className="space-y-2 rounded-md border p-3"
      aria-label="Loading backups"
    >
      {[0, 1, 2, 3, 4].map((row) => (
        <Skeleton key={row} className="h-8 w-full" />
      ))}
    </div>
  )
}

function FormatBadge({ format }: { format?: string | null }) {
  if (!format)
    return (
      <Badge variant="secondary" className="text-xs">
        unknown
      </Badge>
    )
  if (format === 'walg')
    return (
      <Badge className="text-xs bg-emerald-600 hover:bg-emerald-700">
        WAL-G
      </Badge>
    )
  if (format === 'mariadb_physical')
    return (
      <Badge className="text-xs bg-emerald-600 hover:bg-emerald-700">
        mariadb-backup
      </Badge>
    )
  if (format === 'mariadb_dump')
    return (
      <Badge variant="secondary" className="text-xs">
        mysqldump
      </Badge>
    )
  return (
    <Badge variant="secondary" className="text-xs">
      {format}
    </Badge>
  )
}

function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let v = n
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i++
  }
  return `${v.toFixed(i === 0 ? 0 : 1)} ${units[i]}`
}
