// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { AlertTriangle, Loader2 } from 'lucide-react'
import { toast } from 'sonner'
import { client } from '@/api/client/client.gen'
import {
  getSettingsOptions,
  wireguardMeshStatusGetOptions,
} from '@/api/client/@tanstack/react-query.gen'
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
import type { WireguardMeshStatusResponse } from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { useClusterCaFingerprint } from '@/hooks/useClusterCaFingerprint'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'

const ROTATE_CLUSTER_CA_CONFIRMATION = 'ROTATE CLUSTER CA'

/**
 * Typed-confirmation dialog that rotates the cluster CA. The server also
 * requires a full Admin with a fresh MFA-verified session, and compares the
 * fingerprint shown here against the active one.
 */
export function RotateClusterCaDialog({
  open,
  onOpenChange,
  description,
  actionLabel = 'Rotate and disconnect workers',
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  description: ReactNode
  actionLabel?: string
}) {
  const queryClient = useQueryClient()
  const [confirmation, setConfirmation] = useState('')
  const [rotationPending, setRotationPending] = useState(false)
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const { fingerprint } = useClusterCaFingerprint()

  const close = () => {
    onOpenChange(false)
    setConfirmation('')
  }

  const rotateClusterCa = async () => {
    if (!fingerprint || confirmation !== ROTATE_CLUSTER_CA_CONFIRMATION) return

    setRotationPending(true)
    try {
      const response = await client.post({
        url: '/settings/cluster-ca/rotate' as never,
        body: {
          expected_fingerprint: fingerprint,
          confirmation,
        } as never,
      })
      if (response.error) {
        if (
          handleSensitiveActionError(response.error, () => {
            void rotateClusterCa()
          })
        ) {
          // Avoid stacking two modal focus traps while the MFA step-up dialog
          // is active. Keep the typed confirmation for the one-shot retry.
          onOpenChange(false)
          return
        }
        const problem = response.error as {
          detail?: string
          title?: string
        }
        toast.error(
          problem.detail ?? problem.title ?? 'Failed to rotate the cluster CA'
        )
        return
      }

      const result = response.data as
        | { new_fingerprint?: string; revoked_enrollment_tokens?: number }
        | undefined
      toast.success(
        `Cluster CA rotated. ${result?.revoked_enrollment_tokens ?? 0} enrollment token(s) revoked.`
      )
      close()
      // A new CA starts with certificate history, which can unblock the mesh.
      await Promise.all([
        queryClient.invalidateQueries({
          queryKey: getSettingsOptions().queryKey,
        }),
        queryClient.invalidateQueries({
          queryKey: wireguardMeshStatusGetOptions().queryKey,
        }),
      ])
    } catch {
      toast.error('Failed to rotate the cluster CA')
    } finally {
      setRotationPending(false)
    }
  }

  return (
    <>
      <AlertDialog
        open={open}
        onOpenChange={(next) => {
          onOpenChange(next)
          if (!next) setConfirmation('')
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Rotate the cluster CA?</AlertDialogTitle>
            <AlertDialogDescription>{description}</AlertDialogDescription>
          </AlertDialogHeader>
          <div className="space-y-3">
            <p className="break-all rounded-md border bg-muted/30 p-3 font-mono text-xs">
              {fingerprint}
            </p>
            <label className="space-y-2 text-sm font-medium">
              <span>
                Type{' '}
                <span className="font-mono">
                  {ROTATE_CLUSTER_CA_CONFIRMATION}
                </span>
              </span>
              <Input
                autoComplete="off"
                value={confirmation}
                onChange={(event) => setConfirmation(event.target.value)}
                placeholder={ROTATE_CLUSTER_CA_CONFIRMATION}
              />
            </label>
          </div>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={rotationPending} onClick={close}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-white hover:bg-destructive/90"
              disabled={
                rotationPending ||
                !fingerprint ||
                confirmation !== ROTATE_CLUSTER_CA_CONFIRMATION
              }
              onClick={(event) => {
                event.preventDefault()
                void rotateClusterCa()
              }}
            >
              {rotationPending && (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              )}
              {actionLabel}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      {verificationDialog}
    </>
  )
}

/**
 * The cluster CA predates certificate address history, so no mesh pool can be
 * proven free of the addresses its worker certificates name. Rotating it is
 * the fix; say what that costs on this cluster.
 */
export function MeshCaRotationRequired({
  mesh,
}: {
  mesh: WireguardMeshStatusResponse
}) {
  const [open, setOpen] = useState(false)
  const { fingerprint, isLoading } = useClusterCaFingerprint()
  const workers = mesh.nodes.length
  const cost =
    workers === 0
      ? 'No workers are enrolled, so nothing needs re-enrolling.'
      : `The ${workers} enrolled worker${workers === 1 ? '' : 's'} stop${workers === 1 ? 's' : ''} authenticating until re-enrolled with a new join command.`
  return (
    <div className="space-y-2">
      <Alert className="border-amber-500/30 bg-amber-500/5">
        <AlertTriangle className="h-4 w-4 text-amber-500" />
        <AlertTitle className="text-amber-700 dark:text-amber-400">
          Rotate the cluster CA first
        </AlertTitle>
        <AlertDescription className="text-amber-700 dark:text-amber-300">
          The private network would use{' '}
          <code className="font-mono">{mesh.pool_cidr}</code>, and that range is
          fine. But this cluster&apos;s CA was created before Temps recorded
          which addresses its worker certificates name, and those certificates
          are never revoked, so Temps cannot prove none of them covers the
          range. A new CA starts with a complete record. {cost}
        </AlertDescription>
      </Alert>
      <Button
        size="sm"
        variant="outline"
        onClick={() => setOpen(true)}
        disabled={!fingerprint || isLoading}
      >
        Rotate cluster CA
      </Button>
      <RotateClusterCaDialog
        open={open}
        onOpenChange={setOpen}
        actionLabel={workers === 0 ? 'Rotate cluster CA' : undefined}
        description={`${cost} Outstanding join commands stop working. Afterwards, turn on the private network.`}
      />
    </div>
  )
}
