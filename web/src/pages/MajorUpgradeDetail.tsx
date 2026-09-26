// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import {
  cancelPgUpgrade,
  getPgUpgrade,
  getPgUpgradeLogs,
  isTerminal,
  PG_UPGRADE_PHASES,
  PHASE_LABELS,
  phaseIndex,
  retryPgUpgrade,
  rollbackPgUpgrade,
  type PgUpgrade,
  type PgUpgradePhase,
} from '@/lib/pg-upgrades'
import {
  Button,
  Callout,
  Detail,
  PageState,
  Status,
  fmtDateTime,
  fmtRelativeTime,
  type DetailFact,
  type StatusTone,
} from '@temps-sdk/ds'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowLeft,
  Ban,
  CheckCircle2,
  Circle,
  Loader2,
  RefreshCcw,
  RotateCcw,
  XCircle,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link, useParams } from 'react-router'
import { toast } from 'sonner'

// Mirrors `statusVariant`'s old Badge mapping — the Detail template's single
// verdict, derived from the record's own status field.
const UPGRADE_STATUS_VERDICT: Record<string, { tone: StatusTone; label: string }> = {
  completed: { tone: 'ok', label: 'Completed' },
  failed: { tone: 'error', label: 'Failed' },
  running: { tone: 'running', label: 'Running' },
  cancelled: { tone: 'idle', label: 'Cancelled' },
  pending: { tone: 'idle', label: 'Pending' },
}

interface PhaseRowProps {
  phase: PgUpgradePhase
  state: 'done' | 'current' | 'pending' | 'failed'
}

function PhaseRow({ phase, state }: PhaseRowProps) {
  const icon =
    state === 'done' ? (
      <CheckCircle2 className="h-4 w-4 text-success" />
    ) : state === 'current' ? (
      <Loader2 className="h-4 w-4 animate-spin text-primary" />
    ) : state === 'failed' ? (
      <XCircle className="h-4 w-4 text-destructive" />
    ) : (
      <Circle className="h-4 w-4 text-muted-foreground" />
    )

  return (
    <li className="flex items-center gap-3 py-1.5">
      {icon}
      <span
        className={
          state === 'pending'
            ? 'text-sm text-muted-foreground'
            : 'text-sm font-medium'
        }
      >
        {PHASE_LABELS[phase]}
      </span>
    </li>
  )
}

function MajorUpgradeDetailSkeleton({ backAction }: { backAction: React.ReactNode }) {
  return (
    <Detail
      title={<div className="h-7 w-56 animate-pulse rounded bg-muted" />}
      actions={backAction}
      facts={[0, 1, 2, 3].map(() => ({
        label: <div className="h-3 w-16 animate-pulse rounded bg-muted" />,
        value: <div className="h-4 w-24 animate-pulse rounded bg-muted" />,
      }))}
      main={
        <>
          <div className="h-48 w-full animate-pulse rounded-lg bg-muted" />
          <div className="h-64 w-full animate-pulse rounded-lg bg-muted" />
        </>
      }
      aside={<div className="h-40 w-full animate-pulse rounded-lg bg-muted" />}
    />
  )
}

export function MajorUpgradeDetail() {
  const { id, upgradeId } = useParams<{ id: string; upgradeId: string }>()
  const serviceIdNum = id ? parseInt(id, 10) : NaN
  const upgradeIdNum = upgradeId ? parseInt(upgradeId, 10) : NaN
  const queryClient = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [showRollbackDialog, setShowRollbackDialog] = useState(false)
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  usePageTitle(`Upgrade #${upgradeId}`)

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Databases', href: '/storage' },
      { label: `Service ${id}`, href: `/storage/${id}` },
      { label: `Upgrade #${upgradeId}` },
    ])
  }, [id, upgradeId, setBreadcrumbs])

  const upgradeQuery = useQuery<PgUpgrade>({
    queryKey: ['pg-upgrades', serviceIdNum, upgradeIdNum],
    queryFn: () => getPgUpgrade(serviceIdNum, upgradeIdNum),
    enabled: Number.isFinite(serviceIdNum) && Number.isFinite(upgradeIdNum),
    refetchInterval: (query) => {
      const status = query.state.data?.status
      return status && isTerminal(status) ? false : 2000
    },
  })

  const logsQuery = useQuery({
    queryKey: ['pg-upgrades', serviceIdNum, upgradeIdNum, 'logs'],
    queryFn: () => getPgUpgradeLogs(serviceIdNum, upgradeIdNum),
    enabled: Number.isFinite(serviceIdNum) && Number.isFinite(upgradeIdNum),
    refetchInterval: () => {
      const status = upgradeQuery.data?.status
      return status && isTerminal(status) ? false : 3000
    },
  })

  const retryMutation = useMutation({
    mutationFn: () => retryPgUpgrade(serviceIdNum, upgradeIdNum),
    onSuccess: () => {
      toast.success('Retry scheduled')
      queryClient.invalidateQueries({
        queryKey: ['pg-upgrades', serviceIdNum, upgradeIdNum],
      })
    },
    onError: (error: Error) => {
      toast.error('Failed to retry upgrade', { description: error.message })
    },
  })

  const cancelMutation = useMutation({
    mutationFn: () => cancelPgUpgrade(serviceIdNum, upgradeIdNum),
    onSuccess: () => {
      toast.success('Cancellation requested')
      queryClient.invalidateQueries({
        queryKey: ['pg-upgrades', serviceIdNum, upgradeIdNum],
      })
    },
    onError: (error: Error) => {
      toast.error('Failed to cancel upgrade', { description: error.message })
    },
  })

  const rollbackMutation = useMutation({
    mutationFn: () => rollbackPgUpgrade(serviceIdNum, upgradeIdNum),
    onSuccess: () => {
      toast.success('Rollback complete', {
        description:
          'The service has been restored to its pre-upgrade PGDATA volume.',
      })
      queryClient.invalidateQueries({
        queryKey: ['pg-upgrades', serviceIdNum, upgradeIdNum],
      })
      setShowRollbackDialog(false)
    },
    onError: (error: unknown) => {
      if (
        handleSensitiveActionError(error, () => rollbackMutation.mutate())
      ) {
        setShowRollbackDialog(false)
        return
      }
      const msg =
        error instanceof Error
          ? error.message
          : (error as { detail?: string })?.detail ?? 'Unknown error'
      toast.error('Failed to roll back upgrade', { description: msg })
    },
  })

  const backAction = (
    <Button variant="ghost" size="icon" asChild>
      <Link to={`/storage/${id}`}>
        <ArrowLeft className="h-4 w-4" />
      </Link>
    </Button>
  )

  if (!Number.isFinite(serviceIdNum) || !Number.isFinite(upgradeIdNum)) {
    return (
      <PageState
        variant="failed"
        icon={XCircle}
        title="Invalid upgrade"
        description="This upgrade id isn't valid."
        action={backAction}
      />
    )
  }

  if (upgradeQuery.isLoading) {
    return <MajorUpgradeDetailSkeleton backAction={backAction} />
  }

  if (upgradeQuery.isError || !upgradeQuery.data) {
    return (
      <PageState
        variant="failed"
        icon={XCircle}
        title="Couldn't load upgrade"
        description={
          (upgradeQuery.error as Error | undefined)?.message ??
          'Upgrade not found.'
        }
        action={<Button onClick={() => void upgradeQuery.refetch()}>Retry</Button>}
      />
    )
  }

  const upgrade = upgradeQuery.data
  const currentPhaseIdx = phaseIndex(upgrade.phase)
  const phaseState = (idx: number): PhaseRowProps['state'] => {
    if (upgrade.status === 'completed') return 'done'
    if (idx < currentPhaseIdx) return 'done'
    if (idx === currentPhaseIdx) {
      if (upgrade.status === 'failed' || upgrade.status === 'cancelled') return 'failed'
      return 'current'
    }
    return 'pending'
  }

  const verdict =
    UPGRADE_STATUS_VERDICT[upgrade.status] ?? { tone: 'idle' as StatusTone, label: upgrade.status }

  const facts: DetailFact[] = [
    { label: 'Attempt', value: upgrade.attempt },
    { label: 'Phase', value: PHASE_LABELS[upgrade.phase as PgUpgradePhase] ?? upgrade.phase },
    {
      label: 'Started',
      value: upgrade.started_at ? (
        <span title={fmtDateTime(upgrade.started_at)}>
          {fmtRelativeTime(upgrade.started_at)}
        </span>
      ) : (
        'Not started'
      ),
    },
    {
      label: 'Created',
      value: (
        <span title={fmtDateTime(upgrade.created_at)}>
          {fmtRelativeTime(upgrade.created_at)}
        </span>
      ),
    },
  ]

  return (
    <>
      <Detail
        title={`Major Upgrade #${upgrade.id}`}
        description={`PostgreSQL ${upgrade.from_version} → ${upgrade.to_version}`}
        verdict={<Status tone={verdict.tone} label={verdict.label} />}
        facts={facts}
        actions={
          <>
            {backAction}
            {upgrade.status === 'pending' || upgrade.status === 'running' ? (
              <Button
                size="sm"
                variant="outline"
                onClick={() => cancelMutation.mutate()}
                busy={cancelMutation.isPending}
                busyLabel="Cancelling…"
              >
                <Ban className="h-4 w-4 mr-2" />
                Cancel
              </Button>
            ) : null}
            {upgrade.status === 'failed' || upgrade.status === 'cancelled' ? (
              <Button
                size="sm"
                onClick={() => retryMutation.mutate()}
                busy={retryMutation.isPending}
                busyLabel="Retrying…"
              >
                <RefreshCcw className="h-4 w-4 mr-2" />
                Retry
              </Button>
            ) : null}
            {upgrade.status === 'completed' && upgrade.rollback_volume_name ? (
              <Button
                size="sm"
                variant="destructive"
                onClick={() => setShowRollbackDialog(true)}
                disabled={rollbackMutation.isPending}
              >
                <RotateCcw className="h-4 w-4 mr-2" />
                Roll back
              </Button>
            ) : null}
          </>
        }
        main={
          <>
            {upgrade.error_message ? (
              <Callout tone="error" title="Upgrade error">
                <span className="break-all font-mono text-xs">{upgrade.error_message}</span>
              </Callout>
            ) : null}

            <Card>
              <CardHeader>
                <CardTitle>Phases</CardTitle>
                <CardDescription>
                  Each phase is idempotent; failures mark this row as failed at
                  the phase shown, and a retry resumes from that same phase.
                </CardDescription>
              </CardHeader>
              <CardContent>
                <ul className="divide-y">
                  {PG_UPGRADE_PHASES.map((phase, idx) => (
                    <PhaseRow key={phase} phase={phase} state={phaseState(idx)} />
                  ))}
                </ul>
              </CardContent>
            </Card>

            <Card>
              <CardHeader>
                <CardTitle>Logs</CardTitle>
                <CardDescription>
                  JSONL log stream (<code className="text-xs">{upgrade.log_id}</code>).
                  {isTerminal(upgrade.status)
                    ? ' Streaming stopped.'
                    : ' Auto-refreshing every 3s.'}
                </CardDescription>
              </CardHeader>
              <CardContent>
                <pre className="text-xs bg-muted rounded-md p-3 max-h-[480px] overflow-auto whitespace-pre-wrap break-all">
                  {logsQuery.data?.content?.trim() || '(no log output yet)'}
                </pre>
              </CardContent>
            </Card>
          </>
        }
        aside={
          <>
            <Card>
              <CardHeader>
                <CardTitle>Images</CardTitle>
              </CardHeader>
              <CardContent className="space-y-3 text-sm">
                <div>
                  <p className="text-xs text-muted-foreground">From</p>
                  <code className="text-xs break-all">{upgrade.from_image}</code>
                </div>
                <div>
                  <p className="text-xs text-muted-foreground">To</p>
                  <code className="text-xs break-all">{upgrade.to_image}</code>
                </div>
              </CardContent>
            </Card>

            <Card>
              <CardHeader>
                <CardTitle>Rollback info</CardTitle>
              </CardHeader>
              <CardContent className="text-sm space-y-2">
                <div>
                  <span className="text-muted-foreground">Pre-upgrade backup: </span>
                  {upgrade.pre_upgrade_backup_id ? (
                    <span className="font-mono">#{upgrade.pre_upgrade_backup_id}</span>
                  ) : (
                    <span className="text-muted-foreground">(not taken yet)</span>
                  )}
                </div>
                <div>
                  <span className="text-muted-foreground">Rollback volume: </span>
                  {upgrade.rollback_volume_name ? (
                    <code className="text-xs">{upgrade.rollback_volume_name}</code>
                  ) : (
                    <span className="text-muted-foreground">(not created yet)</span>
                  )}
                </div>
                <p className="text-xs text-muted-foreground">
                  The rollback volume is retained for 7 days before it is swept.
                </p>
              </CardContent>
            </Card>
          </>
        }
      />

      <AlertDialog
        open={showRollbackDialog}
        onOpenChange={(open: boolean) => {
          if (!rollbackMutation.isPending) setShowRollbackDialog(open)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Roll back this upgrade?</AlertDialogTitle>
            <AlertDialogDescription>
              This will stop the live PostgreSQL container and replace its data
              volume with the pre-upgrade snapshot. The upgrade is reversed and
              all data written after the upgrade was applied will be
              permanently lost. This action cannot be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={rollbackMutation.isPending}>
              Keep upgraded version
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={(e: { preventDefault: () => void }) => {
                e.preventDefault()
                rollbackMutation.mutate()
              }}
              disabled={rollbackMutation.isPending}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {rollbackMutation.isPending ? 'Rolling back…' : 'Roll back'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {verificationDialog}
    </>
  )
}
