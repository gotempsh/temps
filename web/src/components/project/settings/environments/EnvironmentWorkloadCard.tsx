// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import {
  useMutation,
  useQuery,
  useQueryClient,
  type UseQueryResult,
} from '@tanstack/react-query'
import { Callout } from '@temps-sdk/ds'
import { Boxes, Loader2, RefreshCw } from 'lucide-react'
import { toast } from 'sonner'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ConfirmDialog } from '@/components/ui/confirm-dialog'
import { SettingsSection } from '@/components/ui/settings-section'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { useIsInstanceAdmin } from '@/hooks/useIsInstanceAdmin'
import { problemDetail } from '@/lib/api-problem'
import { cn } from '@/lib/utils'
import {
  DEFAULT_RECOMMENDATION_WINDOW_DAYS,
  WORKLOAD_PLANE_SECTION_ID,
  environmentVolumesQueryKey,
  explainPlacement,
  getPinningRecommendation,
  getWorkloadIsolation,
  getWorkloadMode,
  isPermissionDenied,
  isolationSummary,
  nodeLabel,
  nodeNamesOf,
  pinningRecommendationQueryKey,
  placementExplanationQueryKey,
  placementSummary,
  recommendationSummary,
  setWorkloadIsolation,
  setWorkloadMode,
  workloadIsolationQueryKey,
  workloadModeQueryKey,
  workloadModeSummary,
  type EnvironmentWorkloadIsolation,
  type EnvironmentWorkloadIsolationSettings,
  type EnvironmentWorkloadMode,
  type PinningRecommendation,
  type PlacementExplanation,
} from '@/lib/environment-workload'

const MODES: {
  value: EnvironmentWorkloadMode
  title: string
  description: string
}[] = [
  {
    value: 'legacy',
    title: 'Legacy',
    description:
      'Deployments are placed and run by the existing deployment path. This is the rollback.',
  },
  {
    value: 'shadow',
    title: 'Shadow',
    description:
      'The existing path still decides; the workload plane scheduler runs alongside and every disagreement is logged.',
  },
  {
    value: 'enforce',
    title: 'Enforce',
    description:
      'The workload plane scheduler places replicas, and the workload plane creates and runs them.',
  },
]

const ISOLATIONS: {
  value: EnvironmentWorkloadIsolation
  title: string
  description: string
}[] = [
  {
    value: 'process',
    title: 'Container',
    description:
      'A container sharing the host kernel, like every deployment today.',
  },
  {
    value: 'micro_vm',
    title: 'Firecracker microVM',
    description:
      'Its own kernel. Placed only on a node whose runtime provides Firecracker; refused with the reason otherwise.',
  },
]

const TITLE = 'Workload Plane'
const DESCRIPTION =
  "Choose how this environment's deployments are placed and run, how they are isolated, and see where the next deployment would run and why."

const ENFORCE_CONSEQUENCE =
  "From the next deployment, the workload plane scheduler places this environment's replicas and the workload plane creates and runs them instead of the existing deployment path. A node pin that cannot be met fails the deployment instead of falling back to another node. Containers that are running now are not touched. Switch back to Legacy at any time to roll back."

const MICRO_VM_CONSEQUENCE =
  'From the next deployment, each replica runs in a Firecracker microVM instead of a container. It is placed only on a node whose runtime provides Firecracker; if no eligible node does, the deployment is refused with the scheduler\'s reason. Check "Where the next deployment runs" after saving.'

/**
 * An environment's deployment workload mode, isolation and the scheduler's
 * explanation of where its next deployment would run (ADR-055 Phases 5, 10).
 * Instance-admin only, like the API behind it.
 */
export function EnvironmentWorkloadCard({
  environmentId,
}: {
  environmentId: number
}) {
  const isAdmin = useIsInstanceAdmin()
  const queryClient = useQueryClient()
  const [confirmMode, setConfirmMode] =
    useState<EnvironmentWorkloadMode | null>(null)
  const [confirmIsolation, setConfirmIsolation] =
    useState<EnvironmentWorkloadIsolation | null>(null)
  const [writeDenied, setWriteDenied] = useState<string | null>(null)

  const modeKey = workloadModeQueryKey(environmentId)
  const isolationKey = workloadIsolationQueryKey(environmentId)
  const modeQuery = useQuery({
    queryKey: modeKey,
    queryFn: () => getWorkloadMode(environmentId),
    enabled: isAdmin,
  })
  const isolationQuery = useQuery({
    queryKey: isolationKey,
    queryFn: () => getWorkloadIsolation(environmentId),
    enabled: isAdmin,
  })

  const onWriteError = (title: string) => (error: unknown) => {
    const detail = problemDetail(error, `${title}.`)
    if (isPermissionDenied(error)) setWriteDenied(detail)
    toast.error(title, { description: detail })
  }

  const refreshDependents = () => {
    void queryClient.invalidateQueries({
      queryKey: placementExplanationQueryKey(environmentId),
    })
    void queryClient.invalidateQueries({
      queryKey: environmentVolumesQueryKey(environmentId),
    })
  }

  const updateMode = useMutation({
    mutationFn: (mode: EnvironmentWorkloadMode) =>
      setWorkloadMode(environmentId, mode),
    onSuccess: (settings) => {
      queryClient.setQueryData(modeKey, settings)
      // `applies` on the isolation follows the mode.
      void queryClient.invalidateQueries({ queryKey: isolationKey })
      refreshDependents()
      const title =
        MODES.find((mode) => mode.value === settings.workload_mode)?.title ??
        settings.workload_mode
      toast.success(`Workload mode set to ${title}`)
    },
    onError: onWriteError('Could not change the workload mode'),
  })

  const updateIsolation = useMutation({
    mutationFn: (isolation: EnvironmentWorkloadIsolation) =>
      setWorkloadIsolation(environmentId, isolation),
    onSuccess: (settings) => {
      queryClient.setQueryData(isolationKey, settings)
      refreshDependents()
      toast.success(
        settings.applies
          ? 'Isolation updated'
          : 'Isolation saved; it applies once the workload mode is Enforce'
      )
    },
    onError: onWriteError('Could not change the isolation'),
  })

  const chooseMode = (mode: EnvironmentWorkloadMode) => {
    if (modeQuery.data?.workload_mode === mode) return
    if (mode === 'enforce') setConfirmMode(mode)
    else updateMode.mutate(mode)
  }

  const chooseIsolation = (
    isolation: EnvironmentWorkloadIsolation,
    current: EnvironmentWorkloadIsolationSettings
  ) => {
    if (current.workload_isolation === isolation) return
    if (isolation === 'micro_vm' && current.applies) {
      setConfirmIsolation(isolation)
    } else {
      updateIsolation.mutate(isolation)
    }
  }

  const dialogs = (
    <>
      <ConfirmDialog
        open={confirmMode !== null}
        onOpenChange={(open) => {
          if (!open) setConfirmMode(null)
        }}
        title="Run this environment's deployments on the workload plane?"
        description={ENFORCE_CONSEQUENCE}
        confirmLabel="Switch to Enforce"
        destructive={false}
        onConfirm={() => {
          if (confirmMode) updateMode.mutate(confirmMode)
          setConfirmMode(null)
        }}
      />
      <ConfirmDialog
        open={confirmIsolation !== null}
        onOpenChange={(open) => {
          if (!open) setConfirmIsolation(null)
        }}
        title="Run deployments in a Firecracker microVM?"
        description={MICRO_VM_CONSEQUENCE}
        confirmLabel="Use microVM isolation"
        destructive={false}
        onConfirm={() => {
          if (confirmIsolation) updateIsolation.mutate(confirmIsolation)
          setConfirmIsolation(null)
        }}
      />
    </>
  )

  if (!isAdmin) {
    return (
      <Section>
        <AdminOnlyNotice />
      </Section>
    )
  }

  if (modeQuery.isPending) {
    return (
      <Section>
        <div className="space-y-3" aria-busy="true">
          <Skeleton className="h-20 w-full" />
          <Skeleton className="h-20 w-full" />
          <Skeleton className="h-32 w-full" />
        </div>
      </Section>
    )
  }

  if (modeQuery.isError) {
    return (
      <Section>
        {isPermissionDenied(modeQuery.error) ? (
          <AdminOnlyNotice
            detail={problemDetail(modeQuery.error, 'Permission denied.')}
          />
        ) : (
          <ErrorAlert
            title="Could not load the workload mode"
            description={problemDetail(
              modeQuery.error,
              'The workload mode could not be loaded.'
            )}
            retry={() => modeQuery.refetch()}
            retrying={modeQuery.isFetching}
          />
        )}
      </Section>
    )
  }

  const mode = modeQuery.data.workload_mode
  const readOnly = writeDenied !== null
  return (
    <Section>
      {dialogs}
      <div className="space-y-6">
        {readOnly ? (
          <Callout tone="warning" title="Read-only">
            {writeDenied}
          </Callout>
        ) : null}

        <div className="space-y-3">
          <h4 className="text-sm font-medium">Workload mode</h4>
          <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
            {MODES.map((option) => (
              <button
                key={option.value}
                type="button"
                disabled={updateMode.isPending || readOnly}
                aria-pressed={mode === option.value}
                onClick={() => chooseMode(option.value)}
                className={cn(
                  'rounded-lg border p-4 text-left transition-colors hover:bg-muted/50 disabled:opacity-60',
                  mode === option.value && 'border-primary bg-primary/5'
                )}
              >
                <div className="flex items-center gap-2 font-medium">
                  {option.title}
                  {updateMode.isPending &&
                  updateMode.variables === option.value ? (
                    <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />
                  ) : null}
                </div>
                <p className="mt-1 text-sm text-muted-foreground">
                  {option.description}
                </p>
              </button>
            ))}
          </div>
          <p className="text-sm text-muted-foreground" role="status">
            {workloadModeSummary(mode)}
          </p>
        </div>

        <div className="space-y-3">
          <h4 className="text-sm font-medium">Isolation</h4>
          <IsolationChoice
            query={isolationQuery}
            pending={updateIsolation.isPending}
            pendingValue={updateIsolation.variables}
            readOnly={readOnly}
            onChoose={chooseIsolation}
          />
        </div>

        <PlacementPanel environmentId={environmentId} />
      </div>
    </Section>
  )
}

function Section({ children }: { children: ReactNode }) {
  return (
    <SettingsSection
      id={WORKLOAD_PLANE_SECTION_ID}
      title={TITLE}
      description={DESCRIPTION}
      icon={Boxes}
    >
      {children}
    </SettingsSection>
  )
}

function AdminOnlyNotice({ detail }: { detail?: string }) {
  return (
    <Callout tone="info" title="Instance administrators only">
      <p>
        An instance administrator decides how this environment&apos;s
        deployments are placed and run: the existing path, a shadow comparison,
        or the workload plane, optionally in a Firecracker microVM. Ask an
        administrator to change it.
      </p>
      {detail ? <p className="mt-1 text-muted-foreground">{detail}</p> : null}
    </Callout>
  )
}

function IsolationChoice({
  query,
  pending,
  pendingValue,
  readOnly,
  onChoose,
}: {
  query: UseQueryResult<EnvironmentWorkloadIsolationSettings, Error>
  pending: boolean
  pendingValue: EnvironmentWorkloadIsolation | undefined
  readOnly: boolean
  onChoose: (
    isolation: EnvironmentWorkloadIsolation,
    current: EnvironmentWorkloadIsolationSettings
  ) => void
}) {
  if (query.isPending) {
    return <Skeleton className="h-20 w-full" aria-busy="true" />
  }
  if (query.isError) {
    return (
      <ErrorAlert
        title="Could not load the isolation"
        description={problemDetail(
          query.error,
          'The isolation setting could not be loaded.'
        )}
        retry={() => query.refetch()}
        retrying={query.isFetching}
      />
    )
  }
  const settings = query.data
  return (
    <div className="space-y-3">
      <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
        {ISOLATIONS.map((option) => (
          <button
            key={option.value}
            type="button"
            disabled={pending || readOnly}
            aria-pressed={settings.workload_isolation === option.value}
            onClick={() => onChoose(option.value, settings)}
            className={cn(
              'rounded-lg border p-4 text-left transition-colors hover:bg-muted/50 disabled:opacity-60',
              settings.workload_isolation === option.value &&
                'border-primary bg-primary/5'
            )}
          >
            <div className="flex items-center gap-2 font-medium">
              {option.title}
              {pending && pendingValue === option.value ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />
              ) : null}
            </div>
            <p className="mt-1 text-sm text-muted-foreground">
              {option.description}
            </p>
          </button>
        ))}
      </div>
      {settings.applies ? (
        <p className="text-sm text-muted-foreground" role="status">
          {isolationSummary(settings)}
        </p>
      ) : (
        <Callout tone="warning" title="Not applied in this mode">
          {isolationSummary(settings)}
        </Callout>
      )}
    </div>
  )
}

/** "Where would the next deployment run, and why": read-only. */
function PlacementPanel({ environmentId }: { environmentId: number }) {
  const windowDays = DEFAULT_RECOMMENDATION_WINDOW_DAYS
  const explainQuery = useQuery({
    queryKey: placementExplanationQueryKey(environmentId),
    queryFn: () => explainPlacement(environmentId),
  })
  const recommendationQuery = useQuery({
    queryKey: pinningRecommendationQueryKey(environmentId, windowDays),
    queryFn: () => getPinningRecommendation(environmentId, windowDays),
  })
  const refreshing = explainQuery.isFetching || recommendationQuery.isFetching

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between gap-2">
        <h4 className="text-sm font-medium">Where the next deployment runs</h4>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={refreshing}
          onClick={() => {
            void explainQuery.refetch()
            void recommendationQuery.refetch()
          }}
        >
          <RefreshCw
            className={cn('h-4 w-4 sm:mr-2', refreshing && 'animate-spin')}
          />
          <span className="max-sm:sr-only">Refresh</span>
        </Button>
      </div>
      <ExplanationView query={explainQuery} />
      <RecommendationView
        query={recommendationQuery}
        explanation={explainQuery.data}
      />
    </div>
  )
}

function ExplanationView({
  query,
}: {
  query: UseQueryResult<PlacementExplanation, Error>
}) {
  if (query.isPending) {
    return (
      <div className="space-y-2" aria-busy="true">
        <Skeleton className="h-5 w-3/4" />
        <Skeleton className="h-16 w-full" />
      </div>
    )
  }
  if (query.isError) {
    return (
      <ErrorAlert
        title="Could not explain the placement"
        description={problemDetail(
          query.error,
          'The scheduler explanation could not be loaded.'
        )}
        retry={() => query.refetch()}
        retrying={query.isFetching}
      />
    )
  }
  const explanation = query.data
  const names = nodeNamesOf(explanation)
  return (
    <div className="space-y-3 rounded-lg border p-4">
      {explanation.unschedulable ? (
        <Callout tone="error" title="The next deployment cannot be placed">
          <p>{explanation.unschedulable}</p>
          {explanation.unschedulable_reason ? (
            <p className="mt-1 font-mono text-xs text-muted-foreground">
              {explanation.unschedulable_reason}
            </p>
          ) : null}
        </Callout>
      ) : (
        <p className="text-sm" role="status">
          {placementSummary(explanation)}
        </p>
      )}
      {explanation.pinned_by ? (
        <p className="text-sm text-muted-foreground">
          Pinned: {explanation.pinned_by}
        </p>
      ) : null}
      {explanation.warnings.length > 0 ? (
        <Callout tone="warning" title="Warnings">
          <ul className="list-disc space-y-1 pl-4">
            {explanation.warnings.map((warning) => (
              <li key={warning}>{warning}</li>
            ))}
          </ul>
        </Callout>
      ) : null}

      {explanation.recommended.length > 0 ? (
        <div className="space-y-2">
          <p className="text-xs font-medium uppercase text-muted-foreground">
            Eligible nodes, best first
          </p>
          <ul className="divide-y rounded-md border">
            {explanation.recommended.map((node) => (
              <li
                key={node.node_id}
                className="flex flex-col gap-1 p-3 sm:flex-row sm:items-start sm:justify-between"
              >
                <div className="min-w-0 space-y-1">
                  <div className="text-sm font-medium">
                    {nodeLabel(node.node_id, names)}
                  </div>
                  {node.reasons.length > 0 ? (
                    <p className="text-sm text-muted-foreground">
                      {node.reasons.join('; ')}
                    </p>
                  ) : null}
                </div>
                <Badge variant="outline" className="w-fit shrink-0">
                  score {node.score.toFixed(2)}
                </Badge>
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      {explanation.excluded.length > 0 ? (
        <div className="space-y-2">
          <p className="text-xs font-medium uppercase text-muted-foreground">
            Excluded or kept with a warning
          </p>
          <ul className="divide-y rounded-md border">
            {explanation.excluded.map((node) => (
              <li
                key={`${node.node_id}:${node.reason}`}
                className="flex flex-col gap-1 p-3 sm:flex-row sm:items-start sm:justify-between"
              >
                <div className="min-w-0 space-y-1">
                  <div className="text-sm font-medium">
                    {nodeLabel(node.node_id, names)}
                  </div>
                  <p className="text-sm text-muted-foreground">
                    {node.message}
                  </p>
                </div>
                <Badge
                  variant={
                    node.excluded ? 'destructive-subtle' : 'warning-subtle'
                  }
                  className="w-fit shrink-0 font-mono"
                >
                  {node.excluded ? node.reason : `${node.reason} (kept)`}
                </Badge>
              </li>
            ))}
          </ul>
        </div>
      ) : null}
    </div>
  )
}

function RecommendationView({
  query,
  explanation,
}: {
  query: UseQueryResult<PinningRecommendation, Error>
  explanation: PlacementExplanation | undefined
}) {
  if (query.isPending) {
    return <Skeleton className="h-12 w-full" aria-busy="true" />
  }
  if (query.isError) {
    return (
      <ErrorAlert
        title="Could not load the pinning recommendation"
        description={problemDetail(
          query.error,
          'The pinning recommendation could not be loaded.'
        )}
        retry={() => query.refetch()}
        retrying={query.isFetching}
      />
    )
  }
  const recommendation = query.data
  const names = nodeNamesOf(explanation)
  return (
    <div className="space-y-1 rounded-lg border p-4">
      <p className="text-sm font-medium">Pinning recommendation</p>
      <p className="text-sm text-muted-foreground">
        {recommendationSummary(recommendation, names)}
      </p>
      {recommendation.reasons.length > 0 ? (
        <ul className="list-disc space-y-1 pl-4 text-sm text-muted-foreground">
          {recommendation.reasons.map((reason) => (
            <li key={reason}>{reason}</li>
          ))}
        </ul>
      ) : null}
      {recommendation.changes_placement ? (
        <p className="text-xs text-muted-foreground">
          Recommendations are never applied automatically. To pin, set the
          target nodes in this environment&apos;s configuration.
        </p>
      ) : null}
    </div>
  )
}
