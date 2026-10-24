// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import {
  keepPreviousData,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import {
  Loader2,
  Moon,
  Plus,
  ShieldAlert,
  Sun,
  Trash2,
  Zap,
} from 'lucide-react'
import { useSearchParams } from 'react-router'
import { toast } from 'sonner'
import type { ProjectResponse } from '@/api/client'
import { CreateFunctionDialog } from '@/components/functions/CreateFunctionDialog'
import { FunctionsOnboarding } from '@/components/functions/FunctionsOnboarding'
import { PageHeader } from '@/components/layout/PageContainer'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { CopyButton } from '@/components/ui/copy-button'
import { EmptyState } from '@/components/ui/empty-state'
import { ReadFailure } from '@/components/ui/read-failure'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
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
import { problemDetail } from '@/lib/api-problem'
import {
  canSleep,
  canWake,
  deleteFunction,
  functionStateBadge,
  functionsKeys,
  functionUrl,
  getFunctionsCapability,
  listFunctions,
  sleepFunction,
  wakeFunction,
  type FunctionList,
  type FunctionRecord,
  type FunctionsCapability,
} from '@/lib/functions'
import { isForbiddenRead } from '@/lib/read-failure'

const PAGE_SIZE_OPTIONS = [20, 50, 100] as const

/** Until the capability loads, no runtime is assumed. */
const UNKNOWN_CAPABILITY: FunctionsCapability = {
  configured: false,
  wasm: { available: false, reason: 'Checking the function runtimes…' },
  oci: { available: false, reason: 'Checking the function runtimes…' },
  reason: null,
  setup_path: null,
}

const TONE_VARIANT = {
  success: 'success-subtle',
  pending: 'warning-subtle',
  neutral: 'neutral-subtle',
  warning: 'destructive-subtle',
} as const

function positiveInt(value: string | null, fallback: number): number {
  const parsed = Number(value)
  return Number.isInteger(parsed) && parsed > 0 ? parsed : fallback
}

function artifactText(fn: FunctionRecord): string {
  if (fn.artifact_kind === 'oci') return fn.image_reference ?? 'OCI image'
  const digest = fn.wasm_digest ? fn.wasm_digest.slice(0, 19) : 'WASM module'
  const size =
    fn.wasm_size_bytes != null
      ? ` · ${(fn.wasm_size_bytes / 1024).toFixed(0)} KB`
      : ''
  return `${digest}${size}`
}

function SkeletonRows() {
  return (
    <>
      {[0, 1, 2].map((row) => (
        <TableRow key={row}>
          {[0, 1, 2, 3, 4].map((cell) => (
            <TableCell key={cell}>
              <Skeleton className="h-4 w-full max-w-[140px]" />
            </TableCell>
          ))}
        </TableRow>
      ))}
    </>
  )
}

/**
 * A project's functions (ADR-055 §14): WASM modules or OCI images served on
 * their own hostname, running only while requests arrive.
 */
export function ProjectFunctions({ project }: { project: ProjectResponse }) {
  const { setBreadcrumbs } = useBreadcrumbs()
  usePageTitle(`Functions · ${project.name}`)
  useEffect(() => {
    setBreadcrumbs([
      { label: project.name, href: `/projects/${project.slug}/project` },
      { label: 'Functions' },
    ])
  }, [setBreadcrumbs, project.name, project.slug])

  const queryClient = useQueryClient()
  const [searchParams, setSearchParams] = useSearchParams()
  const page = positiveInt(searchParams.get('page'), 1)
  const pageSize = Math.min(positiveInt(searchParams.get('page_size'), 20), 100)
  const [createOpen, setCreateOpen] = useState(false)
  const [toDelete, setToDelete] = useState<FunctionRecord | null>(null)
  /** The server's refusal after a write answered 403: actions hide. */
  const [refusal, setRefusal] = useState<string | null>(null)

  const setParam = (key: string, value: string | null) =>
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev)
        if (value === null) next.delete(key)
        else next.set(key, value)
        return next
      },
      { replace: true }
    )

  const capabilityQuery = useQuery({
    queryKey: functionsKeys.capability,
    queryFn: getFunctionsCapability,
    meta: { errorTitle: 'Could not check the function runtimes' },
  })
  const listKey = functionsKeys.list(project.id, page, pageSize)
  const listQuery = useQuery({
    queryKey: listKey,
    queryFn: () => listFunctions(project.id, page, pageSize),
    placeholderData: keepPreviousData,
    meta: { errorTitle: 'Could not load functions' },
  })

  const handleForbidden = (error: unknown): boolean => {
    if (!isForbiddenRead(error)) return false
    setRefusal(
      problemDetail(error, 'Your role cannot change functions in this project.')
    )
    return true
  }

  const replaceRow = (fn: FunctionRecord) =>
    queryClient.setQueryData<FunctionList>(listKey, (previous) =>
      previous
        ? {
            ...previous,
            functions: previous.functions.map((row) =>
              row.id === fn.id ? fn : row
            ),
          }
        : previous
    )

  const wake = useMutation({
    mutationFn: (fn: FunctionRecord) => wakeFunction(project.id, fn.id),
    meta: { errorTitle: 'Could not wake the function' },
    onSuccess: (fn) => {
      replaceRow(fn)
      toast.success(`${fn.name} is awake`)
    },
    onError: (error, fn) => {
      handleForbidden(error)
      toast.error(`Could not wake ${fn.name}`, {
        description: problemDetail(error, 'It could not be started.'),
      })
      void queryClient.invalidateQueries({
        queryKey: functionsKeys.project(project.id),
      })
    },
  })
  const sleep = useMutation({
    mutationFn: (fn: FunctionRecord) => sleepFunction(project.id, fn.id),
    meta: { errorTitle: 'Could not put the function to sleep' },
    onSuccess: (result) => {
      replaceRow(result.function)
      toast.success(
        result.slept
          ? `${result.function.name} is asleep`
          : `${result.function.name} was not running`
      )
    },
    onError: (error, fn) => {
      handleForbidden(error)
      toast.error(`Could not put ${fn.name} to sleep`, {
        description: problemDetail(error, 'It could not be stopped.'),
      })
    },
  })
  const remove = useMutation({
    mutationFn: (fn: FunctionRecord) => deleteFunction(project.id, fn.id),
    meta: { errorTitle: 'Could not delete the function' },
    onSuccess: async (_, fn) => {
      toast.success(`Function ${fn.name} deleted`)
      setToDelete(null)
      if ((listQuery.data?.functions.length ?? 1) === 1 && page > 1) {
        setParam('page', String(page - 1))
      }
      await queryClient.invalidateQueries({
        queryKey: functionsKeys.project(project.id),
      })
    },
    onError: (error, fn) => {
      if (handleForbidden(error)) setToDelete(null)
      toast.error(`Could not delete ${fn.name}`, {
        description: problemDetail(error, 'The server refused the delete.'),
      })
    },
  })

  const capability = capabilityQuery.data
  const configured = capability?.configured === true
  const canAct = refusal === null
  const total = listQuery.data?.total ?? 0
  const totalPages = listQuery.data?.total_pages ?? 1
  const protocol =
    typeof window === 'undefined' ? 'https:' : window.location.protocol

  return (
    <div className="w-full min-w-0 space-y-6">
      <PageHeader
        title="Functions"
        description="Serve a WASM module or an OCI image on its own hostname. It runs only while requests arrive."
        actions={
          configured && canAct ? (
            <Button onClick={() => setCreateOpen(true)}>
              <Plus className="mr-2 h-4 w-4" />
              New function
            </Button>
          ) : null
        }
      />

      {refusal ? (
        <Alert>
          <ShieldAlert className="h-4 w-4" />
          <AlertTitle>Read-only</AlertTitle>
          <AlertDescription>{refusal}</AlertDescription>
        </Alert>
      ) : null}

      {capabilityQuery.isPending ? (
        <Skeleton className="h-24 w-full" />
      ) : capabilityQuery.isError ? (
        <ReadFailure
          resource="Function runtimes"
          error={capabilityQuery.error}
          onRetry={
            isForbiddenRead(capabilityQuery.error)
              ? undefined
              : () => void capabilityQuery.refetch()
          }
          retrying={capabilityQuery.isFetching}
        />
      ) : configured ? null : (
        <FunctionsOnboarding capability={capabilityQuery.data} />
      )}

      <div className="rounded-lg border bg-card text-card-foreground">
        {listQuery.isError && !listQuery.data ? (
          <div className="p-4">
            <ReadFailure
              resource="Functions"
              error={listQuery.error}
              embedded
              onRetry={
                isForbiddenRead(listQuery.error)
                  ? undefined
                  : () => void listQuery.refetch()
              }
              retrying={listQuery.isFetching}
            />
          </div>
        ) : !listQuery.isPending && total === 0 ? (
          <EmptyState
            size="compact"
            icon={Zap}
            title="No functions yet"
            description={
              configured
                ? 'Create one to serve a WASM module or an OCI image on its own hostname.'
                : 'Enable a function runtime first; the section above says what is missing.'
            }
            action={
              configured && canAct ? (
                <Button onClick={() => setCreateOpen(true)}>
                  <Plus className="mr-2 h-4 w-4" />
                  New function
                </Button>
              ) : undefined
            }
          />
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead>State</TableHead>
                <TableHead>URL</TableHead>
                <TableHead className="hidden md:table-cell">Artifact</TableHead>
                <TableHead className="text-right">
                  <span className="sr-only">Actions</span>
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {listQuery.isPending ? (
                <SkeletonRows />
              ) : (
                (listQuery.data?.functions ?? []).map((fn) => {
                  const badge = functionStateBadge(fn.state)
                  const url = functionUrl(fn.hostname, protocol)
                  const waking = wake.isPending && wake.variables?.id === fn.id
                  const sleeping =
                    sleep.isPending && sleep.variables?.id === fn.id
                  return (
                    <TableRow key={fn.id}>
                      <TableCell>
                        <div className="font-medium">{fn.name}</div>
                        <div className="text-xs text-muted-foreground">
                          {fn.artifact_kind === 'oci' ? 'OCI' : 'WASM'} · port{' '}
                          {fn.port} · sleeps after {fn.idle_timeout_seconds}s
                        </div>
                      </TableCell>
                      <TableCell>
                        <Badge variant={TONE_VARIANT[badge.tone]}>
                          {badge.label}
                        </Badge>
                        {fn.ready_message || fn.ready_reason ? (
                          <div className="mt-1 max-w-[260px] text-xs text-muted-foreground">
                            {fn.ready_message ?? fn.ready_reason}
                          </div>
                        ) : null}
                      </TableCell>
                      <TableCell className="min-w-0">
                        {fn.hostname ? (
                          <div className="flex items-center gap-1">
                            <a
                              href={url}
                              target="_blank"
                              rel="noreferrer"
                              className="truncate font-mono text-xs underline underline-offset-2"
                            >
                              {fn.hostname}
                            </a>
                            <CopyButton
                              value={url}
                              minimal
                              label={`Copy the URL of ${fn.name}`}
                            />
                          </div>
                        ) : (
                          <span className="text-xs text-muted-foreground">
                            No hostname yet
                          </span>
                        )}
                      </TableCell>
                      <TableCell className="hidden md:table-cell">
                        <span
                          className="block max-w-[220px] truncate font-mono text-xs text-muted-foreground"
                          title={
                            fn.image_reference ?? fn.wasm_digest ?? undefined
                          }
                        >
                          {artifactText(fn)}
                        </span>
                      </TableCell>
                      <TableCell className="text-right">
                        {canAct ? (
                          <div className="flex justify-end gap-1">
                            {canWake(fn.state) ? (
                              <Button
                                variant="outline"
                                size="sm"
                                disabled={waking}
                                onClick={() => wake.mutate(fn)}
                              >
                                {waking ? (
                                  <Loader2 className="h-4 w-4 animate-spin" />
                                ) : (
                                  <Sun className="h-4 w-4" />
                                )}
                                <span className="ml-1 max-sm:sr-only">
                                  Wake
                                </span>
                              </Button>
                            ) : null}
                            {canSleep(fn.state) ? (
                              <Button
                                variant="outline"
                                size="sm"
                                disabled={sleeping}
                                onClick={() => sleep.mutate(fn)}
                              >
                                {sleeping ? (
                                  <Loader2 className="h-4 w-4 animate-spin" />
                                ) : (
                                  <Moon className="h-4 w-4" />
                                )}
                                <span className="ml-1 max-sm:sr-only">
                                  Sleep
                                </span>
                              </Button>
                            ) : null}
                            <Button
                              variant="ghost"
                              size="icon"
                              aria-label={`Delete function ${fn.name}`}
                              onClick={() => setToDelete(fn)}
                            >
                              <Trash2 className="h-4 w-4" />
                            </Button>
                          </div>
                        ) : null}
                      </TableCell>
                    </TableRow>
                  )
                })
              )}
            </TableBody>
          </Table>
        )}
      </div>

      {total > 0 ? (
        <ResponsivePagination
          page={page}
          pageSize={pageSize}
          total={total}
          totalPages={totalPages}
          pageSizeOptions={PAGE_SIZE_OPTIONS}
          onPageChange={(next) => setParam('page', String(next))}
          onPageSizeChange={(size) =>
            setSearchParams(
              (prev) => {
                const next = new URLSearchParams(prev)
                next.set('page_size', String(size))
                next.delete('page')
                return next
              },
              { replace: true }
            )
          }
          ariaLabel="Functions pagination"
        />
      ) : null}

      <CreateFunctionDialog
        key={capability ? 'capability-loaded' : 'capability-pending'}
        open={createOpen && configured}
        onOpenChange={setCreateOpen}
        projectId={project.id}
        capability={capability ?? UNKNOWN_CAPABILITY}
        onForbidden={handleForbidden}
      />
      <ConfirmDeleteDialog
        open={toDelete !== null}
        onOpenChange={(open) => {
          if (!open) setToDelete(null)
        }}
        resourceType="function"
        resourceName={toDelete?.name ?? ''}
        consequences={
          toDelete
            ? `Its instance is removed and ${toDelete.hostname} stops answering.`
            : undefined
        }
        isPending={remove.isPending}
        onConfirm={() => {
          if (toDelete) remove.mutate(toDelete)
        }}
      />
    </div>
  )
}
