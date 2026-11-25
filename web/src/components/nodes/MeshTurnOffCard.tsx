// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Loader2, PowerOff } from 'lucide-react'
import { toast } from 'sonner'
import {
  adminListNodesOptions,
  wireguardMeshDisableMutation,
  wireguardMeshStatusGetOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { WireguardMeshStatusResponse } from '@/api/client/types.gen'
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
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { problemDetail } from '@/lib/api-problem'
import { meshTurnOff } from '@/lib/wireguard-mesh'

/**
 * Turn the WireGuard mesh off. Shown whenever the mesh state is known; the
 * server refuses while anything depends on the mesh, and that reason is shown
 * next to the disabled button instead of being discovered on click.
 */
export function MeshTurnOffCard({
  mesh,
}: {
  mesh: WireguardMeshStatusResponse | undefined
}) {
  const queryClient = useQueryClient()
  const [confirmOpen, setConfirmOpen] = useState(false)
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const disable = useMutation({
    ...wireguardMeshDisableMutation(),
    onSuccess: async (status) => {
      // A status read already in flight would land after this and show the
      // mesh as still on, so cancel it before writing the new state.
      const statusKey = wireguardMeshStatusGetOptions().queryKey
      await queryClient.cancelQueries({ queryKey: statusKey })
      queryClient.setQueryData(statusKey, status)
      toast.success('WireGuard mesh turned off', {
        description:
          'The control plane stops tending its end; the interface goes away when temps restarts.',
      })
      await queryClient.invalidateQueries({
        queryKey: adminListNodesOptions().queryKey,
      })
    },
    onError: (error, variables) => {
      if (handleSensitiveActionError(error, () => disable.mutate(variables)))
        return
      toast.error('Could not turn off the WireGuard mesh', {
        description: problemDetail(
          error,
          'Check your permissions and try again.'
        ),
      })
    },
  })

  const state = mesh ? meshTurnOff(mesh) : undefined

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <PowerOff className="h-5 w-5" />
          Turn off the mesh
        </CardTitle>
        <CardDescription>
          Possible only while no node, pairing or cluster network depends on it.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3 text-sm">
        {state?.kind === 'off' ? (
          <p className="text-muted-foreground">The WireGuard mesh is off.</p>
        ) : (
          <>
            {state?.kind === 'blocked' && (
              <p className="text-muted-foreground">{state.reason}</p>
            )}
            <Button
              variant="outline"
              onClick={() => setConfirmOpen(true)}
              disabled={state?.kind !== 'available' || disable.isPending}
            >
              {disable.isPending ? (
                <Loader2 className="mr-1 h-4 w-4 animate-spin" />
              ) : (
                <PowerOff className="mr-1 h-4 w-4" />
              )}
              Turn off mesh
            </Button>
          </>
        )}
      </CardContent>

      <AlertDialog open={confirmOpen} onOpenChange={setConfirmOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Turn off the WireGuard mesh?</AlertDialogTitle>
            <AlertDialogDescription>
              Nodes can no longer join over the internet until it is turned on
              again. The control plane&apos;s WireGuard interface goes away the
              next time temps restarts.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction onClick={() => disable.mutate({})}>
              Turn off mesh
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      {verificationDialog}
    </Card>
  )
}
