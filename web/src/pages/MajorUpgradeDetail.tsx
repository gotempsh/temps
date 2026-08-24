import { Alert, AlertDescription } from '@/components/ui/alert'
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
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertTriangle,
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

function statusVariant(status: string): 'default' | 'secondary' | 'destructive' | 'outline' {
  switch (status) {
    case 'completed':
      return 'default'
    case 'failed':
      return 'destructive'
    case 'running':
      return 'secondary'
    case 'cancelled':
      return 'outline'
    case 'pending':
      return 'outline'
    default:
      return 'outline'
  }
}

interface PhaseRowProps {
  phase: PgUpgradePhase
  state: 'done' | 'current' | 'pending' | 'failed'
}

function PhaseRow({ phase, state }: PhaseRowProps) {
  const icon =
    state === 'done' ? (
      <CheckCircle2 className="h-4 w-4 text-green-600" />
    ) : state === 'current' ? (
      <Loader2 className="h-4 w-4 animate-spin text-blue-600" />
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

  if (!Number.isFinite(serviceIdNum) || !Number.isFinite(upgradeIdNum)) {
    return (
      <div className="p-4 sm:p-6">
        <Alert variant="destructive">
          <AlertDescription>Invalid upgrade id.</AlertDescription>
        </Alert>
      </div>
    )
  }

  if (upgradeQuery.isLoading) {
    return (
      <div className="p-6 flex items-center gap-2 text-muted-foreground">
        <Loader2 className="h-4 w-4 animate-spin" />
        Loading upgrade…
      </div>
    )
  }

  if (upgradeQuery.isError || !upgradeQuery.data) {
    return (
      <div className="p-4 sm:p-6">
        <Alert variant="destructive">
          <AlertDescription>
            {(upgradeQuery.error as Error | undefined)?.message ??
              'Upgrade not found.'}
          </AlertDescription>
        </Alert>
      </div>
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

  return (
    <div className="p-6 space-y-6 max-w-5xl mx-auto">
      <div className="flex items-center justify-between gap-4">
        <div className="flex items-center gap-3">
          <Button variant="ghost" size="icon" asChild>
            <Link to={`/storage/${id}`}>
              <ArrowLeft className="h-4 w-4" />
            </Link>
          </Button>
          <div>
            <h1 className="text-2xl font-semibold">
              Major Upgrade #{upgrade.id}
            </h1>
            <p className="text-sm text-muted-foreground">
              PostgreSQL {upgrade.from_version} → {upgrade.to_version}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <Badge variant={statusVariant(upgrade.status)}>{upgrade.status}</Badge>
          {upgrade.status === 'pending' || upgrade.status === 'running' ? (
            <Button
              size="sm"
              variant="outline"
              onClick={() => cancelMutation.mutate()}
              disabled={cancelMutation.isPending}
            >
              {cancelMutation.isPending ? (
                <Loader2 className="h-4 w-4 mr-2 animate-spin" />
              ) : (
                <Ban className="h-4 w-4 mr-2" />
              )}
              Cancel
            </Button>
          ) : null}
          {upgrade.status === 'failed' || upgrade.status === 'cancelled' ? (
            <Button
              size="sm"
              onClick={() => retryMutation.mutate()}
              disabled={retryMutation.isPending}
            >
              {retryMutation.isPending ? (
                <Loader2 className="h-4 w-4 mr-2 animate-spin" />
              ) : (
                <RefreshCcw className="h-4 w-4 mr-2" />
              )}
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
              {rollbackMutation.isPending ? (
                <Loader2 className="h-4 w-4 mr-2 animate-spin" />
              ) : (
                <RotateCcw className="h-4 w-4 mr-2" />
              )}
              Roll back
            </Button>
          ) : null}
        </div>
      </div>

      {upgrade.error_message ? (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertDescription className="font-mono text-xs break-all">
            {upgrade.error_message}
          </AlertDescription>
        </Alert>
      ) : null}

      <div className="grid gap-6 md:grid-cols-3">
        <Card>
          <CardHeader>
            <CardTitle>From</CardTitle>
          </CardHeader>
          <CardContent>
            <code className="text-xs break-all">{upgrade.from_image}</code>
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>To</CardTitle>
          </CardHeader>
          <CardContent>
            <code className="text-xs break-all">{upgrade.to_image}</code>
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>Attempt</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-semibold">{upgrade.attempt}</div>
            <p className="text-xs text-muted-foreground">
              Retries preserve phase so work is not repeated.
            </p>
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Phases</CardTitle>
          <CardDescription>
            Each phase is idempotent; failures mark this row as failed at the
            phase shown, and a retry resumes from that same phase.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <ul className="divide-y">
            {PG_UPGRADE_PHASES.map((phase, idx) => (
              <PhaseRow
                key={phase}
                phase={phase}
                state={phaseState(idx)}
              />
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
    </div>
  )
}
