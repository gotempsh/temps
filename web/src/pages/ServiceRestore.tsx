import {
  getRestoreCapabilitiesOptions,
  getRestoreRunOptions,
  getServiceOptions,
  listS3SourcesOptions,
  listSourceBackupsOptions,
  planRestoreMutation,
  startRestoreMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ExternalServiceDetails,
  RestoreCapabilitiesResponse,
  RestorePlan,
  RestoreRunView,
  SourceBackupEntry,
} from '@/api/client/types.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
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
import { useQuery, useMutation } from '@tanstack/react-query'
import {
  AlertCircle,
  AlertTriangle,
  ArrowLeft,
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  Clock,
  Database,
  Loader2,
  RotateCcw,
  Search,
  Star,
  XCircle,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'

type Mode = 'in_place' | 'new_service' | 'pitr'

const PHASES: Array<{ id: string; label: string }> = [
  { id: 'prepare', label: 'Prepare' },
  { id: 'provision', label: 'Provision' },
  { id: 'restore', label: 'Restore data' },
  { id: 'recover', label: 'Recover WAL' },
  { id: 'verify', label: 'Verify' },
  { id: 'completed', label: 'Completed' },
]

// Engine-family check — mirror of the backend `engines_compatible` helper in
// crates/temps-backup/src/services/restore.rs. S3-compatible object stores
// all share the mc-mirror restore path and are mutually restorable.
const OBJECT_STORE_FAMILY = new Set(['s3', 'rustfs', 'minio', 'blob'])
function enginesCompatible(
  backupEngine: string | null | undefined,
  targetEngine: string,
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
  const navigate = useNavigate()
  usePageTitle('Restore service')
  const { setBreadcrumbs } = useBreadcrumbs()

  // ----- Queries ------------------------------------------------------------
  const { data: serviceDetails, isLoading: serviceLoading } = useQuery({
    ...getServiceOptions({ path: { id: serviceId } }),
    enabled: Number.isFinite(serviceId),
  })
  const service = (serviceDetails as ExternalServiceDetails | undefined)?.service

  const { data: caps } = useQuery({
    ...getRestoreCapabilitiesOptions({ path: { id: serviceId } }),
    enabled: Number.isFinite(serviceId),
  })
  const capabilities = caps as RestoreCapabilitiesResponse | undefined

  const { data: s3Sources } = useQuery({
    ...listS3SourcesOptions(),
    enabled: Number.isFinite(serviceId),
  })
  const defaultSource = useMemo(
    () =>
      s3Sources?.find((s) => (s as { is_default?: boolean }).is_default === true),
    [s3Sources],
  )

  // ----- Local state --------------------------------------------------------
  const [selectedSourceId, setSelectedSourceId] = useState<number | undefined>()
  const [selectedBackup, setSelectedBackup] = useState<SourceBackupEntry | undefined>()
  const [mode, setMode] = useState<Mode>('in_place')
  const [newServiceName, setNewServiceName] = useState('')
  const [pitrTargetTime, setPitrTargetTime] = useState('')
  const [pitrToNewService, setPitrToNewService] = useState(false)
  const [confirmText, setConfirmText] = useState('')
  const [search, setSearch] = useState('')
  const [runningRunId, setRunningRunId] = useState<number | null>(null)

  // Breadcrumbs
  useEffect(() => {
    if (!service) return
    setBreadcrumbs([
      { label: 'Databases', href: '/storage' },
      { label: service.name, href: `/storage/${serviceId}` },
      { label: 'Restore' },
    ])
    return () => setBreadcrumbs([])
  }, [service, serviceId, setBreadcrumbs])

  // Default S3 source on first load
  useEffect(() => {
    if (defaultSource && selectedSourceId === undefined) {
      setSelectedSourceId(defaultSource.id)
    }
  }, [defaultSource, selectedSourceId])

  // Seed auto-suggested new service name from capabilities
  useEffect(() => {
    if (capabilities?.suggested_new_service_name && newServiceName === '') {
      setNewServiceName(capabilities.suggested_new_service_name)
    }
  }, [capabilities?.suggested_new_service_name, newServiceName])

  // ----- Backups list ------------------------------------------------------
  const {
    data: backupIndex,
    isLoading: backupsLoading,
    error: backupsError,
    refetch: refetchBackups,
  } = useQuery({
    ...listSourceBackupsOptions({ path: { id: selectedSourceId ?? 0 } }),
    enabled: selectedSourceId !== undefined,
  })

  const allBackups: SourceBackupEntry[] =
    (backupIndex as { backups?: SourceBackupEntry[] } | undefined)?.backups ?? []

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
    Math.ceil(filteredBackups.length / BACKUPS_PAGE_SIZE),
  )

  useEffect(() => {
    if (backupsPage > backupsTotalPages) {
      setBackupsPage(backupsTotalPages)
    }
  }, [backupsPage, backupsTotalPages])

  useEffect(() => {
    setBackupsPage(1)
  }, [search, selectedSourceId])

  const paginatedBackups = useMemo(
    () =>
      filteredBackups.slice(
        (backupsPage - 1) * BACKUPS_PAGE_SIZE,
        backupsPage * BACKUPS_PAGE_SIZE,
      ),
    [filteredBackups, backupsPage],
  )

  const backupsPageWindow = useMemo(() => {
    const windowSize = Math.min(5, backupsTotalPages)
    const start = Math.max(
      1,
      Math.min(
        backupsPage - Math.floor(windowSize / 2),
        backupsTotalPages - windowSize + 1,
      ),
    )
    return Array.from({ length: windowSize }, (_, idx) => start + idx)
  }, [backupsPage, backupsTotalPages])

  // Incompat backups (dropped above) — count for context so user isn't confused
  const incompatCount = useMemo(() => {
    const engine = (service?.service_type ?? '').toLowerCase()
    if (!engine) return 0
    return allBackups.filter((b) => !enginesCompatible(b.engine, engine)).length
  }, [allBackups, service?.service_type])

  // ----- Run polling --------------------------------------------------------
  const { data: runRow } = useQuery({
    ...getRestoreRunOptions({ path: { id: runningRunId ?? 0 } }),
    enabled: runningRunId != null,
    refetchInterval: (query) => {
      const row = query.state.data as RestoreRunView | undefined
      if (!row) return 2000
      return row.status === 'completed' || row.status === 'failed' ? false : 2000
    },
  })

  useEffect(() => {
    if (!runRow || runningRunId == null) return
    if (runRow.status === 'completed') {
      toast.success('Restore completed', {
        description:
          runRow.target_service_id != null
            ? `New service id: ${runRow.target_service_id}`
            : `Restored onto ${service?.name ?? 'service'}.`,
      })
    } else if (runRow.status === 'failed') {
      toast.error('Restore failed', {
        description: runRow.error_message ?? 'Unknown error',
      })
    }
  }, [runRow?.status, runningRunId, runRow, service?.name])

  // ----- Mutations ---------------------------------------------------------
  const startMutation = useMutation({
    ...startRestoreMutation(),
    meta: { errorTitle: 'Failed to start restore' },
    onSuccess: (run) => {
      const r = run as RestoreRunView
      setRunningRunId(r.id)
      toast.success('Restore started', {
        description: `Run ${r.id} (phase: ${r.phase}).`,
      })
    },
  })

  const planMutation = useMutation({
    ...planRestoreMutation(),
    meta: { errorTitle: 'Failed to plan restore' },
  })

  const isOrphan = selectedBackup?.source === 's3_scan'
  const selectedIsWalG = selectedBackup?.format === 'walg'
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
          s3_source_id: selectedSourceId,
        }
      : { backup_id: selectedBackup.id }
    if (mode === 'in_place') return { ...base, mode: 'in_place' }
    if (mode === 'new_service')
      return {
        ...base,
        mode: 'new_service',
        name: newServiceName.trim(),
        parameter_overrides: {},
      }
    // pitr
    if (!pitrTargetTime || Number.isNaN(new Date(pitrTargetTime).getTime()))
      return null
    return {
      ...base,
      mode: 'pitr',
      to_new_service: pitrToNewService,
      new_service_name: pitrToNewService ? newServiceName.trim() : undefined,
      target: { kind: 'time', time: new Date(pitrTargetTime).toISOString() },
    }
  }

  // Re-plan whenever the inputs that affect the plan change. Throttled
  // naturally by React Query's mutate serialization — a rapid change in
  // mode/backup still produces the latest plan shown.
  useEffect(() => {
    const body = buildRequestBody()
    if (!body) return
    planMutation.mutate({
      path: { id: serviceId },
      body: body as never,
    })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    selectedBackup?.id,
    selectedBackup?.location,
    selectedSourceId,
    mode,
    newServiceName,
    pitrTargetTime,
    pitrToNewService,
    serviceId,
  ])

  const plan = planMutation.data as RestorePlan | undefined
  const planError = planMutation.error as Error | null
  const planHasBlockingErrors = !!plan && plan.errors.length > 0

  const canSubmit = (() => {
    if (!selectedBackup) return false
    if (mode === 'new_service' && newServiceName.trim().length === 0) return false
    if (mode === 'pitr') {
      if (!pitrTargetTime || Number.isNaN(new Date(pitrTargetTime).getTime()))
        return false
      if (pitrToNewService && newServiceName.trim().length === 0) return false
      if (!selectedIsWalG) return false
    }
    if (!confirmOk) return false
    // Block on plan errors — user must resolve them (e.g. pick a different
    // backup or target) before we'll even let them click Start.
    if (planHasBlockingErrors) return false
    return true
  })()

  const handleStart = () => {
    if (!selectedBackup) return
    const base: Record<string, unknown> = isOrphan
      ? {
          backup_location: selectedBackup.location,
          backup_engine: selectedBackup.engine,
          s3_source_id: selectedSourceId,
        }
      : { backup_id: selectedBackup.id }

    let body: Record<string, unknown>
    if (mode === 'in_place') {
      body = { ...base, mode: 'in_place' }
    } else if (mode === 'new_service') {
      body = {
        ...base,
        mode: 'new_service',
        name: newServiceName.trim(),
        parameter_overrides: {},
      }
    } else {
      body = {
        ...base,
        mode: 'pitr',
        to_new_service: pitrToNewService,
        new_service_name: pitrToNewService ? newServiceName.trim() : undefined,
        target: { kind: 'time', time: new Date(pitrTargetTime).toISOString() },
      }
    }

    startMutation.mutate({
      path: { id: serviceId },
      body: body as never,
    })
  }

  // ---------- Render --------------------------------------------------------

  if (!Number.isFinite(serviceId)) {
    return (
      <div className="container mx-auto py-8">
        <Alert variant="destructive">
          <AlertCircle className="h-4 w-4" />
          <AlertDescription>Invalid service id.</AlertDescription>
        </Alert>
      </div>
    )
  }

  if (serviceLoading || !service) {
    return (
      <div className="container mx-auto py-12 flex items-center justify-center">
        <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
      </div>
    )
  }

  // Running view — takes over the whole page once a run is in flight.
  if (runningRunId != null) {
    return (
      <div className="container mx-auto py-8 max-w-3xl">
        <PageHeader service={service} />
        <RunProgress run={runRow} />
        <div className="mt-6 flex gap-2">
          <Button
            variant="outline"
            onClick={() => navigate(`/storage/${serviceId}`)}
            disabled={
              runRow?.status !== 'completed' && runRow?.status !== 'failed'
            }
          >
            <ArrowLeft className="h-4 w-4 mr-2" />
            Back to service
          </Button>
          {runRow?.status === 'completed' && runRow.target_service_id != null ? (
            <Button onClick={() => navigate(`/storage/${runRow.target_service_id}`)}>
              <Database className="h-4 w-4 mr-2" />
              Open restored service
            </Button>
          ) : null}
        </div>
      </div>
    )
  }

  // Configure view
  return (
    <div className="container mx-auto py-6 max-w-5xl space-y-6">
      <PageHeader service={service} />

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
          <Select
            value={selectedSourceId?.toString()}
            onValueChange={(v) => {
              setSelectedSourceId(Number(v))
              setSelectedBackup(undefined)
            }}
          >
            <SelectTrigger className="max-w-md">
              <SelectValue placeholder="Select an S3 source" />
            </SelectTrigger>
            <SelectContent>
              {s3Sources?.map((source) => {
                const isDefault =
                  (source as { is_default?: boolean }).is_default === true
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
            All {service.service_type} backups on this source. Backups
            produced by a different service can be selected — useful for
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
                onChange={(e) => setSearch(e.target.value)}
              />
            </div>
            <Button
              variant="outline"
              size="sm"
              onClick={() => refetchBackups()}
              disabled={backupsLoading}
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

          {backupsLoading ? (
            <div className="flex items-center text-sm text-muted-foreground py-8 justify-center">
              <Loader2 className="h-4 w-4 animate-spin mr-2" />
              Loading backups…
            </div>
          ) : backupsError ? (
            <Alert variant="destructive">
              <AlertCircle className="h-4 w-4" />
              <AlertDescription>
                Failed to load backups from this source. Check that the S3
                endpoint and bucket path are correct.
              </AlertDescription>
            </Alert>
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
                        className={`cursor-pointer ${
                          isSel ? 'bg-accent' : ''
                        }`}
                        onClick={() => setSelectedBackup(b)}
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

          {!backupsLoading && !backupsError && backupsTotalPages > 1 && (
            <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
              <div className="text-sm text-muted-foreground">
                <span className="hidden sm:inline tabular-nums">
                  Showing {(backupsPage - 1) * BACKUPS_PAGE_SIZE + 1} to{' '}
                  {Math.min(
                    backupsPage * BACKUPS_PAGE_SIZE,
                    filteredBackups.length,
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
                      variant={pageNum === backupsPage ? 'default' : 'outline'}
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

          {selectedBackup ? (
            <div className="text-xs text-muted-foreground font-mono break-all pt-2">
              Selected: {selectedBackup.location || '(no location)'}
            </div>
          ) : null}

          {isCrossService ? (
            <Alert>
              <AlertTriangle className="h-4 w-4" />
              <AlertDescription>
                This backup was produced by <strong>{selectedBackup?.origin_service_name}</strong>,
                not <strong>{service.name}</strong>. Make sure you intend to
                restore foreign data onto this service.
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
          <RadioGroup
            value={mode}
            onValueChange={(v) => setMode(v as Mode)}
            className="grid gap-3"
          >
            <label
              htmlFor="mode-in-place"
              className={`flex items-start gap-3 rounded-md border p-3 cursor-pointer ${
                mode === 'in_place' ? 'border-primary bg-accent/50' : ''
              } ${capabilities?.restore_in_place === false ? 'opacity-50 cursor-not-allowed' : ''}`}
            >
              <RadioGroupItem
                value="in_place"
                id="mode-in-place"
                disabled={capabilities?.restore_in_place === false}
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
              } ${capabilities?.restore_to_new_service === false ? 'opacity-50 cursor-not-allowed' : ''}`}
            >
              <RadioGroupItem
                value="new_service"
                id="mode-new"
                disabled={capabilities?.restore_to_new_service === false}
                className="mt-0.5"
              />
              <div className="flex-1">
                <div className="font-medium">Clone into a new service</div>
                <div className="text-xs text-muted-foreground mt-0.5">
                  Provisions a sibling of <strong>{service.name}</strong> with the
                  restored data. Original is untouched.
                </div>
              </div>
            </label>

            <label
              htmlFor="mode-pitr"
              className={`flex items-start gap-3 rounded-md border p-3 cursor-pointer ${
                mode === 'pitr' ? 'border-primary bg-accent/50' : ''
              } ${capabilities?.pitr === false || !selectedIsWalG ? 'opacity-50 cursor-not-allowed' : ''}`}
            >
              <RadioGroupItem
                value="pitr"
                id="mode-pitr"
                disabled={capabilities?.pitr === false || !selectedIsWalG}
                className="mt-0.5"
              />
              <div className="flex-1">
                <div className="font-medium">Point-in-time recovery</div>
                <div className="text-xs text-muted-foreground mt-0.5">
                  Recover to a specific timestamp via WAL replay. Requires a
                  WAL-G backup.
                  {selectedBackup && !selectedIsWalG
                    ? ' Selected backup is pg_dump; PITR not available.'
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
                value={newServiceName}
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
                  PostgreSQL will replay archived WAL from the base backup
                  through this time.
                </p>
              </div>
              <div className="flex items-center gap-2">
                <input
                  id="pitr-new"
                  type="checkbox"
                  checked={pitrToNewService}
                  onChange={(e) => setPitrToNewService(e.target.checked)}
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
                    value={newServiceName}
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
    </div>
  )
}

// ----- Smaller pieces ------------------------------------------------------

function PageHeader({
  service,
}: {
  service: { name: string; service_type: string }
}) {
  return (
    <div className="space-y-1">
      <div className="flex items-center gap-2">
        <RotateCcw className="h-5 w-5 text-muted-foreground" />
        <h1 className="text-2xl font-semibold">Restore service</h1>
      </div>
      <p className="text-sm text-muted-foreground">
        Target: <strong>{service.name}</strong>{' '}
        <Badge variant="outline" className="ml-1 text-xs">
          {service.service_type}
        </Badge>
      </p>
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
  return (
    <Badge variant="secondary" className="text-xs">
      {format}
    </Badge>
  )
}

function RunProgress({ run }: { run: RestoreRunView | undefined }) {
  if (!run) {
    return (
      <Card>
        <CardContent className="py-12 flex items-center justify-center text-sm text-muted-foreground">
          <Loader2 className="h-4 w-4 animate-spin mr-2" />
          Starting…
        </CardContent>
      </Card>
    )
  }
  const currentIdx = PHASES.findIndex((p) => p.id === run.phase)
  const isTerminal = run.status === 'completed' || run.status === 'failed'

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center gap-2">
          <Badge
            variant={
              run.status === 'completed'
                ? 'default'
                : run.status === 'failed'
                  ? 'destructive'
                  : 'secondary'
            }
          >
            {run.status}
          </Badge>
          <span className="text-sm text-muted-foreground">
            mode <code>{run.mode}</code> · run #{run.id}
          </span>
        </div>
      </CardHeader>
      <CardContent>
        <ol className="space-y-3">
          {PHASES.map((p, idx) => {
            const state =
              run.status === 'failed' && p.id === run.phase
                ? 'failed'
                : idx < currentIdx || (isTerminal && run.status === 'completed')
                  ? 'done'
                  : idx === currentIdx
                    ? 'active'
                    : 'pending'
            return (
              <li key={p.id} className="flex items-center gap-3 text-sm">
                {state === 'done' ? (
                  <CheckCircle2 className="h-5 w-5 text-green-600" />
                ) : state === 'active' ? (
                  <Loader2 className="h-5 w-5 animate-spin text-blue-600" />
                ) : state === 'failed' ? (
                  <XCircle className="h-5 w-5 text-red-600" />
                ) : (
                  <Clock className="h-5 w-5 text-muted-foreground" />
                )}
                <span
                  className={
                    state === 'pending'
                      ? 'text-muted-foreground'
                      : state === 'failed'
                        ? 'text-red-600'
                        : ''
                  }
                >
                  {p.label}
                </span>
              </li>
            )
          })}
        </ol>
        {run.error_message ? (
          <Alert variant="destructive" className="mt-4">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription className="break-all">
              {run.error_message}
            </AlertDescription>
          </Alert>
        ) : null}
      </CardContent>
    </Card>
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
