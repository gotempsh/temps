// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import {
  useInvalidateUpdateStatus,
  useSelfUpdateCapability,
  useStartSelfUpdate,
} from '@/hooks/useSelfUpdate'
import type {
  SelfUpdateAttempt,
  SelfUpdatePhase,
  SelfUpdateRestartMode,
} from '@/api/client/types.gen'
import {
  AlertTriangle,
  ArrowUpCircle,
  CheckCircle2,
  Loader2,
  Terminal,
  XCircle,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'

/** How often the server is asked where the update has got to. */
const POLL_MS = 2000

/** Extra time allowed on top of the server's own estimate before we stop
 *  waiting and tell the user to go look at the host. */
const RESTART_GRACE_SECS = 60

const PHASE_LABEL: Record<SelfUpdatePhase, string> = {
  idle: 'Starting…',
  resolving: 'Finding the release…',
  downloading: 'Downloading…',
  verifying: 'Verifying checksum…',
  installing: 'Installing…',
  migrating: 'Applying database migrations…',
  restarting: 'Restarting the server…',
  pending_restart: 'Installed — waiting for you to restart temps',
  failed: 'Failed',
}

interface UpdateNowDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** Version tag the server reports as newest, for the confirmation copy. */
  latestVersion?: string | null
  currentVersion?: string | null
}

/**
 * Confirm-and-watch dialog for applying a release from the console.
 *
 * The interesting part is what happens after "Update and restart": the server
 * deliberately stops answering partway through, so this keeps polling across
 * the downtime and reports the recorded outcome once it is back. It never
 * closes itself on success — a stale console bundle is still loaded in the
 * browser, so the user is offered an explicit reload.
 */
export function UpdateNowDialog({
  open,
  onOpenChange,
  latestVersion,
  currentVersion,
}: UpdateNowDialogProps) {
  const [watching, setWatching] = useState(false)
  const [waitedTooLong, setWaitedTooLong] = useState(false)
  // `last_attempt` as it looked before we started, so a result left over from a
  // previous update is never mistaken for this one's.
  const baselineAttemptRef = useRef<string | null>(null)
  const deadlineRef = useRef<number | null>(null)

  const { data: capability, isPending: capabilityPending } =
    useSelfUpdateCapability({
      enabled: open,
      pollMs: watching ? POLL_MS : undefined,
    })
  const startUpdate = useStartSelfUpdate()
  const invalidateUpdateStatus = useInvalidateUpdateStatus()

  const attempt = capability?.last_attempt ?? null
  const isOurResult =
    watching &&
    attempt != null &&
    attempt.status !== 'pending' &&
    attempt.started_at !== baselineAttemptRef.current

  const result: SelfUpdateAttempt | null = isOurResult ? attempt : null

  // Stop polling once the attempt resolves, and refresh the banner's own
  // "update available" state so it disappears after a successful upgrade.
  useEffect(() => {
    if (result) {
      setWatching(false)
      deadlineRef.current = null
      invalidateUpdateStatus()
    }
  }, [result, invalidateUpdateStatus])

  // Give up waiting eventually. A server that never comes back is a real
  // outcome and must be reported as one, not as a spinner that runs forever.
  useEffect(() => {
    if (!watching) {
      setWaitedTooLong(false)
      return
    }
    const timer = setInterval(() => {
      if (deadlineRef.current && Date.now() > deadlineRef.current) {
        setWaitedTooLong(true)
      }
    }, 1000)
    return () => clearInterval(timer)
  }, [watching])

  const blockedReason = useMemo(() => {
    if (!capability) return null
    if (!capability.allowed) {
      return 'Your account does not have permission to update the platform (platform:update).'
    }
    if (!capability.can_apply) {
      return capability.reason ?? 'Updating from the console is not available.'
    }
    return null
  }, [capability])

  const handleStart = async () => {
    baselineAttemptRef.current = capability?.last_attempt?.started_at ?? null
    setWaitedTooLong(false)
    try {
      const started = await startUpdate.mutateAsync({ body: {} })
      deadlineRef.current =
        Date.now() +
        ((started?.estimated_restart_secs ?? 45) + RESTART_GRACE_SECS) * 1000
      setWatching(true)
    } catch {
      // The mutation's error is rendered below; nothing to do here.
    }
  }

  const handleClose = (next: boolean) => {
    if (!next) {
      setWatching(false)
      deadlineRef.current = null
    }
    onOpenChange(next)
  }

  const phase = capability?.phase ?? 'idle'
  const startError = startUpdate.error

  return (
    <AlertDialog open={open} onOpenChange={handleClose}>
      <AlertDialogContent className="max-w-lg">
        <AlertDialogHeader>
          <AlertDialogTitle className="flex items-center gap-2">
            {result?.status === 'succeeded' ||
            result?.status === 'installed_pending_restart' ? (
              <CheckCircle2 className="h-5 w-5 text-green-600" />
            ) : result?.status === 'failed' ? (
              <XCircle className="h-5 w-5 text-destructive" />
            ) : (
              <ArrowUpCircle className="h-5 w-5" />
            )}
            {result?.status === 'succeeded'
              ? 'Update complete'
              : result?.status === 'installed_pending_restart'
                ? 'Update installed'
                : result?.status === 'failed'
                  ? 'Update failed'
                  : watching
                    ? 'Updating temps'
                    : 'Update temps'}
          </AlertDialogTitle>
          <AlertDialogDescription asChild>
            <div className="space-y-3 text-sm">
              {result ? (
                <ResultBody result={result} />
              ) : watching ? (
                <WatchingBody
                  phase={phase}
                  waitedTooLong={waitedTooLong}
                  expectRestart={capability?.restart_mode !== 'manual'}
                  migrationsApplied={capability?.migrations_applied ?? null}
                  migrationsTotal={capability?.migrations_total ?? null}
                  currentMigrationName={capability?.current_migration_name ?? null}
                />
              ) : (
                <ConfirmBody
                  currentVersion={currentVersion}
                  latestVersion={latestVersion}
                  blockedReason={blockedReason}
                  caveat={capability?.caveat ?? null}
                  manualCommand={capability?.manual_command ?? 'temps upgrade'}
                  binaryPath={capability?.binary_path ?? ''}
                  restartMode={capability?.restart_mode}
                  loading={capabilityPending}
                />
              )}

              {startError ? (
                <p className="rounded border border-destructive/40 bg-destructive/10 p-2 text-destructive">
                  {errorDetail(startError)}
                </p>
              ) : null}
            </div>
          </AlertDialogDescription>
        </AlertDialogHeader>

        <AlertDialogFooter>
          {result?.status === 'succeeded' ? (
            <>
              <AlertDialogCancel>Close</AlertDialogCancel>
              <Button onClick={() => window.location.reload()}>
                Reload console
              </Button>
            </>
          ) : result ? (
            <AlertDialogCancel>Close</AlertDialogCancel>
          ) : watching ? (
            <AlertDialogCancel>Hide</AlertDialogCancel>
          ) : (
            <>
              <AlertDialogCancel>Cancel</AlertDialogCancel>
              <Button
                onClick={handleStart}
                disabled={
                  !capability?.can_apply ||
                  !capability?.allowed ||
                  startUpdate.isPending
                }
              >
                {startUpdate.isPending && (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                )}
                {capability?.restart_mode === 'manual'
                  ? 'Install update'
                  : 'Update and restart'}
              </Button>
            </>
          )}
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}

function ConfirmBody({
  currentVersion,
  latestVersion,
  blockedReason,
  caveat,
  manualCommand,
  binaryPath,
  restartMode,
  loading,
}: {
  currentVersion?: string | null
  latestVersion?: string | null
  blockedReason: string | null
  caveat: string | null
  manualCommand: string
  binaryPath: string
  restartMode?: SelfUpdateRestartMode
  loading: boolean
}) {
  const willRestart = restartMode !== 'manual'
  return (
    <>
      <p>
        temps will download{' '}
        <strong className="font-mono">
          {latestVersion ?? 'the latest release'}
        </strong>
        , verify its checksum and replace the binary
        {currentVersion && (
          <>
            {' '}
            (currently <span className="font-mono">{currentVersion}</span>)
          </>
        )}
        {willRestart ? ', then restart.' : '.'}
      </p>
      <p className="text-muted-foreground">
        {willRestart
          ? 'The server — including proxied traffic to your deployed apps — is unavailable for a few seconds while it restarts. The previous binary is kept alongside the new one so you can roll back by hand.'
          : 'Nothing goes offline: temps keeps serving the current version until you restart it yourself, which is when the new version takes over. The previous binary is kept alongside the new one so you can roll back by hand.'}
      </p>

      {caveat && (
        <p className="flex gap-2 rounded border border-amber-300 bg-amber-50 p-2 text-amber-900 dark:border-amber-900/50 dark:bg-amber-950/30 dark:text-amber-200">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
          <span>{caveat}</span>
        </p>
      )}

      {loading ? (
        <p className="text-muted-foreground">Checking this server…</p>
      ) : (
        blockedReason && (
          <div className="space-y-2 rounded border border-amber-300 bg-amber-50 p-2 text-amber-900 dark:border-amber-900/50 dark:bg-amber-950/30 dark:text-amber-200">
            <p className="flex gap-2">
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
              <span>{blockedReason}</span>
            </p>
            <div className="flex items-center gap-2">
              <Terminal className="h-3.5 w-3.5 shrink-0" />
              <code className="min-w-0 flex-1 truncate font-mono text-xs">
                {manualCommand}
              </code>
              <CopyButton value={manualCommand} minimal />
            </div>
          </div>
        )
      )}

      {binaryPath && !blockedReason && (
        <p className="text-xs text-muted-foreground">
          Replaces <span className="font-mono">{binaryPath}</span>
        </p>
      )}
    </>
  )
}

function WatchingBody({
  phase,
  waitedTooLong,
  expectRestart,
  migrationsApplied,
  migrationsTotal,
  currentMigrationName,
}: {
  phase: SelfUpdatePhase
  waitedTooLong: boolean
  expectRestart: boolean
  migrationsApplied: number | null
  migrationsTotal: number | null
  currentMigrationName: string | null
}) {
  return (
    <>
      <p className="flex items-center gap-2">
        <Loader2 className="h-4 w-4 animate-spin" />
        {PHASE_LABEL[phase] ?? 'Working…'}
      </p>

      {phase === 'migrating' && (
        <MigrationProgress
          applied={migrationsApplied}
          total={migrationsTotal}
          currentName={currentMigrationName}
        />
      )}

      <p className="text-muted-foreground">
        {expectRestart
          ? 'The console loses contact with the server while it restarts — that is expected. This dialog reports the result as soon as it is back.'
          : 'temps stays up while this runs; the new version takes over when you restart it.'}
      </p>
      {waitedTooLong && expectRestart && (
        <p className="flex gap-2 rounded border border-destructive/40 bg-destructive/10 p-2 text-destructive">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
          <span>
            The server has not come back yet. Check the service on the host (
            <span className="font-mono">systemctl status temps</span> and{' '}
            <span className="font-mono">journalctl -u temps</span>). The
            previous binary was kept as a{' '}
            <span className="font-mono">.bak</span> file next to the current
            one.
          </span>
        </p>
      )}
    </>
  )
}

function MigrationProgress({
  applied,
  total,
  currentName,
}: {
  applied: number | null
  total: number | null
  currentName: string | null
}) {
  const hasCounts = applied !== null && total !== null && total > 0

  return (
    <div className="space-y-1 rounded border bg-muted/40 p-2 text-xs text-muted-foreground">
      {hasCounts ? (
        <>
          <p className="font-medium text-foreground">
            {applied} of {total} migration{total !== 1 ? 's' : ''} applied
          </p>
          {/* Progress bar */}
          <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
            <div
              className="h-full rounded-full bg-primary transition-all duration-300"
              style={{ width: `${Math.round((applied / total) * 100)}%` }}
            />
          </div>
        </>
      ) : (
        <p>Checking for pending migrations…</p>
      )}
      {currentName && (
        <p className="truncate font-mono" title={currentName}>
          {currentName}
        </p>
      )}
    </div>
  )
}

function ResultBody({ result }: { result: SelfUpdateAttempt }) {
  if (result.status === 'installed_pending_restart') {
    // The most important case to be blunt about: the bytes are on disk but the
    // running server is unchanged, and only the operator can finish the job.
    return (
      <>
        <p>
          <strong className="font-mono">{result.to_version}</strong> is
          installed, but temps is still running{' '}
          <span className="font-mono">{result.from_version}</span>.
        </p>
        <p className="text-muted-foreground">
          Nothing on this host restarts temps automatically, so the update goes
          live the next time you restart it yourself.
        </p>
        {result.previous_binary_path && (
          <p className="text-xs text-muted-foreground">
            Previous binary kept at{' '}
            <span className="font-mono">{result.previous_binary_path}</span>
          </p>
        )}
      </>
    )
  }
  if (result.status === 'succeeded') {
    return (
      <>
        <p>
          temps is now running{' '}
          <strong className="font-mono">{result.to_version}</strong> (was{' '}
          <span className="font-mono">{result.from_version}</span>).
        </p>
        <p className="text-muted-foreground">
          Reload the console to pick up the matching web assets.
        </p>
      </>
    )
  }
  return (
    <>
      <p>{result.error ?? 'The update did not complete.'}</p>
      <p className="text-muted-foreground">
        The server is still running{' '}
        <span className="font-mono">{result.from_version}</span>.
      </p>
    </>
  )
}

/** Pull the Problem Details `detail` out of a failed mutation. */
function errorDetail(error: unknown): string {
  const problem = error as { detail?: string; message?: string } | null
  return problem?.detail ?? problem?.message ?? 'Could not start the update.'
}
