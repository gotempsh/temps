// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { SuggestedFix } from '@/api/client'
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
import { Checkbox } from '@/components/ui/checkbox'
import { Skeleton } from '@/components/ui/skeleton'
import {
  allowsAll,
  useContainerSecurity,
  withCapabilities,
} from '@/hooks/useContainerSecurity'
import { problemDetail } from '@/lib/api-problem'
import { useMutation } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'

/** The setting was saved; starting the new deployment failed. */
class RedeployError extends Error {
  constructor(readonly reason: unknown) {
    super('Could not start the redeploy')
  }
}

function names(capabilities: readonly string[]): string {
  return capabilities.join(' and ')
}

/**
 * The fix for a deployment whose entrypoint needs a Linux capability: allow
 * it for the project (audited, after a confirmation) and redeploy the same
 * source in one action, or change the image. Never applied on its own.
 */
export function EntrypointCapabilityFix({
  projectId,
  fix,
  settingsHref,
  onRedeploy,
}: {
  projectId: number
  fix: SuggestedFix
  /** The project's container security section. */
  settingsHref: string
  /** Redeploys the failed deployment's source; absent when it cannot be. */
  onRedeploy?: () => Promise<void>
}) {
  const { query, save, handleSensitiveActionError, verificationDialog } =
    useContainerSecurity(projectId)
  const [confirmOpen, setConfirmOpen] = useState(false)
  const [acknowledged, setAcknowledged] = useState(false)
  const capabilities = fix.allow_capabilities
  const label = names(capabilities)
  const current = query.data?.allowed_capabilities ?? []
  const alreadyAllowed =
    query.data !== undefined && allowsAll(current, capabilities)

  const apply = useMutation({
    retry: false,
    mutationFn: async () => {
      if (!query.data) throw new Error('Container security is not loaded')
      if (!allowsAll(query.data.allowed_capabilities, capabilities)) {
        await save.mutateAsync({
          allowed: withCapabilities(
            query.data.allowed_capabilities,
            capabilities
          ),
          expected: query.data.allowed_capabilities,
          acknowledgeRisks: true,
        })
      }
      if (!onRedeploy) return
      try {
        await onRedeploy()
      } catch (reason) {
        throw new RedeployError(reason)
      }
    },
    onSuccess: () => {
      setConfirmOpen(false)
      setAcknowledged(false)
      toast.success(
        onRedeploy
          ? `${label} allowed. A new deployment of the same source started.`
          : `${label} allowed. Redeploy to apply it.`
      )
    },
    onError: (error) => {
      if (error instanceof RedeployError) {
        setConfirmOpen(false)
        toast.error(`${label} is allowed, but the redeploy did not start`, {
          description: problemDetail(error.reason, 'Redeploy to apply it.'),
        })
        return
      }
      if (handleSensitiveActionError(error, () => apply.mutate())) return
      toast.error(`Could not allow ${label}`, {
        description: problemDetail(error, 'Try again.'),
      })
    },
  })

  if (capabilities.length === 0) {
    return (
      <div
        className="space-y-1 rounded-md border bg-background p-3 text-sm"
        data-testid="entrypoint-capability-fix"
      >
        <p>
          The entrypoint needs{' '}
          <code>{names(fix.not_allowed_capabilities)}</code>, which no project
          can allow.
        </p>
        <p className="text-muted-foreground">{fix.image_fix}</p>
      </div>
    )
  }

  const step = save.isPending
    ? `Allowing ${label} for this project…`
    : apply.isPending
      ? 'Starting a new deployment of the same source…'
      : null
  const actionLabel = alreadyAllowed
    ? 'Redeploy'
    : onRedeploy
      ? `Allow ${label} for this project and redeploy`
      : `Allow ${label} for this project`

  return (
    <div
      className="space-y-2 rounded-md border bg-background p-3 text-sm"
      data-testid="entrypoint-capability-fix"
    >
      <p>
        The image’s entrypoint needs the Linux capability <code>{label}</code>
        {fix.verified ? '' : ' (most likely; Temps could not read the binary)'},
        which Temps removes from every container by default.
      </p>
      {query.isPending && <Skeleton className="h-7 w-72" />}
      {query.data && !query.data.can_edit && (
        <p className="text-muted-foreground" role="note">
          {query.data.edit_requirement} Ask one to allow {label} in{' '}
          <Link to={settingsHref} className="underline underline-offset-4">
            Container security
          </Link>
          , or change the image: {fix.image_fix}
        </p>
      )}
      {(query.data?.can_edit || query.isError) && (
        <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center">
          {query.data?.can_edit && (
            <Button
              type="button"
              size="sm"
              className="h-7"
              disabled={apply.isPending || (alreadyAllowed && !onRedeploy)}
              onClick={() => {
                if (alreadyAllowed) apply.mutate()
                else {
                  setAcknowledged(false)
                  setConfirmOpen(true)
                }
              }}
            >
              {apply.isPending && (
                <Loader2 className="mr-1.5 h-3 w-3 animate-spin" />
              )}
              {actionLabel}
            </Button>
          )}
          <Button asChild variant="outline" size="sm" className="h-7">
            <Link to={settingsHref}>Container security</Link>
          </Button>
        </div>
      )}
      {alreadyAllowed && !apply.isPending && (
        <p className="text-xs text-muted-foreground">
          {label} is allowed for this project now; the next deployment gets it.
        </p>
      )}
      {step && (
        <p className="text-xs text-muted-foreground" role="status">
          {step}
        </p>
      )}
      <AlertDialog
        open={confirmOpen}
        onOpenChange={(open) => {
          if (!open && !apply.isPending) setConfirmOpen(false)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Allow {label} and redeploy?</AlertDialogTitle>
            <AlertDialogDescription>
              Every container of this project gets {label} back after Temps
              drops all Linux capabilities, from this deployment on. All other
              capabilities stay dropped and no-new-privileges stays on.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <div className="space-y-3 text-sm">
            <p>
              <span className="font-medium">Trade-off: </span>a process in the
              container may listen on ports below 1024 inside its own network
              namespace. It cannot gain other capabilities or reach the host.
            </p>
            <p className="text-muted-foreground">
              <span className="font-medium">Keep full hardening instead: </span>
              {fix.image_fix}
            </p>
            {onRedeploy && (
              <p className="text-muted-foreground">
                Temps then redeploys the same source as this deployment.
              </p>
            )}
            <div className="flex items-start gap-2">
              <Checkbox
                id="acknowledge-entrypoint-capability"
                checked={acknowledged}
                onCheckedChange={(value) => setAcknowledged(value === true)}
                disabled={apply.isPending}
              />
              <label htmlFor="acknowledge-entrypoint-capability">
                I understand this weakens this project’s container hardening.
              </label>
            </div>
            {step && (
              <p className="text-xs text-muted-foreground" role="status">
                {step}
              </p>
            )}
          </div>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={apply.isPending}>
              Cancel
            </AlertDialogCancel>
            <Button
              disabled={!acknowledged || apply.isPending}
              onClick={() => apply.mutate()}
            >
              {apply.isPending && (
                <Loader2 className="mr-1.5 h-3 w-3 animate-spin" />
              )}
              {actionLabel}
            </Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      {verificationDialog}
    </div>
  )
}
