// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Loader2, Waypoints } from 'lucide-react'
import { toast } from 'sonner'
import {
  wireguardMeshHubSetMutation,
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
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import {
  QueryErrorAlert,
  TONE_CLASSES,
  WithCode,
} from '@/components/nodes/mesh-ui'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { problemDetail } from '@/lib/api-problem'
import {
  hubCandidates,
  hubOptionValue,
  hubTargetFromOption,
  linksNeedingAttention,
  meshLinkLabel,
} from '@/lib/wireguard-mesh'

/**
 * The mesh hub (ADR 048 D4): a member that relays between members that
 * cannot reach each other, such as two nodes behind NAT. Shown whenever the
 * mesh exists, so the pairs that cannot connect are visible with their fix.
 */
export function MeshHubCard({
  mesh,
  isLoading,
  error,
  onRetry,
  retrying,
}: {
  mesh: WireguardMeshStatusResponse | undefined
  isLoading: boolean
  error: unknown
  onRetry: () => void
  retrying?: boolean
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Waypoints className="h-5 w-5" />
          Mesh hub
        </CardTitle>
        <CardDescription>
          Two nodes that both sit behind NAT cannot dial each other. A hub — the
          control plane or a node both reach — relays their traffic. Each hop
          stays encrypted, but the hub decrypts what it relays: pick a machine
          you control.
        </CardDescription>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-3">
            <Skeleton className="h-9 w-72" />
            <Skeleton className="h-16 w-full" />
          </div>
        ) : error || !mesh ? (
          <QueryErrorAlert
            title="Could not read the WireGuard mesh state"
            error={error}
            onRetry={onRetry}
            retrying={retrying}
          />
        ) : mesh.state === 'ready' ? (
          // Keyed by the saved hub so the choice follows a change made
          // elsewhere (the CLI, another admin) instead of keeping a stale one.
          <HubSettings key={hubOptionValue(mesh.hub?.target)} mesh={mesh} />
        ) : (
          <p className="text-sm text-muted-foreground">
            {mesh.state === 'starting'
              ? 'Available once the WireGuard mesh is up.'
              : 'Hubs relay traffic on the WireGuard mesh, which is off. Turn it on under “Over the internet” in Worker Nodes above; members that cannot reach each other then show here with how to connect them.'}
          </p>
        )}
      </CardContent>
    </Card>
  )
}

function HubSettings({ mesh }: { mesh: WireguardMeshStatusResponse }) {
  const queryClient = useQueryClient()
  const current = hubOptionValue(mesh.hub?.target)
  const [choice, setChoice] = useState(current)
  const [confirmOpen, setConfirmOpen] = useState(false)
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const setHub = useMutation({
    ...wireguardMeshHubSetMutation(),
    onSuccess: (status) => {
      queryClient.setQueryData(wireguardMeshStatusGetOptions().queryKey, status)
      setChoice(hubOptionValue(status.hub?.target))
      toast.success(
        status.hub ? `${status.hub.name} is the mesh hub` : 'Mesh hub removed',
        {
          description: status.hub
            ? 'Pairs that never connect move onto it within a few minutes.'
            : 'Relayed pairs go back to trying the direct path.',
        }
      )
    },
    onError: (error, variables) => {
      if (handleSensitiveActionError(error, () => setHub.mutate(variables)))
        return
      toast.error('Could not change the mesh hub', {
        description: problemDetail(
          error,
          'Check your permissions and try again.'
        ),
      })
    },
  })

  const candidates = hubCandidates(mesh)
  const chosenLabel =
    candidates.find((option) => option.value === choice)?.label ?? 'no hub'
  const links = linksNeedingAttention(mesh.links)
  const unreachable = mesh.links.filter((link) => link.state === 'unreachable')

  return (
    <div className="space-y-4 text-sm">
      <div className="flex flex-wrap items-center gap-2">
        <Select value={choice} onValueChange={setChoice}>
          <SelectTrigger className="w-60" aria-label="Mesh hub">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="none">No hub</SelectItem>
            {candidates.map((option) => (
              <SelectItem key={option.value} value={option.value}>
                {option.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Button
          onClick={() => setConfirmOpen(true)}
          disabled={choice === current || setHub.isPending}
        >
          {setHub.isPending && (
            <Loader2 className="mr-1 h-4 w-4 animate-spin" />
          )}
          Save
        </Button>
        {!mesh.hub && unreachable.length > 0 && (
          <span className="text-muted-foreground">
            {unreachable.length} pair(s) cannot connect: choose a member both
            sides reach.
          </span>
        )}
      </div>

      {mesh.links.length === 0 ? (
        <p className="rounded-md border border-dashed p-3 text-muted-foreground">
          No other members on the mesh yet: add a node and its links show here.
        </p>
      ) : links.length === 0 ? (
        <p className="text-muted-foreground">
          All {mesh.links.length} pairs of mesh members connect directly.
        </p>
      ) : (
        <ul className="divide-y rounded-md border">
          {links.map((link) => {
            const { label, tone } = meshLinkLabel(link.state)
            return (
              <li key={`${link.a}-${link.b}`} className="space-y-1 p-3">
                <div className="flex items-center gap-2">
                  <span className="font-medium">
                    {link.a} ↔ {link.b}
                  </span>
                  <Badge variant="outline" className={TONE_CLASSES[tone]}>
                    {label}
                  </Badge>
                </div>
                {link.detail && (
                  <p className="text-xs text-muted-foreground">
                    <WithCode text={link.detail} />
                  </p>
                )}
              </li>
            )
          })}
        </ul>
      )}

      <AlertDialog open={confirmOpen} onOpenChange={setConfirmOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              {choice === 'none'
                ? 'Remove the mesh hub?'
                : `Make ${chosenLabel} the mesh hub?`}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {choice === 'none'
                ? 'Members that only reach each other through the hub lose that link.'
                : `${chosenLabel} relays — and can read — the traffic between members that cannot reach each other. Pick a machine you control.`}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() =>
                setHub.mutate({ body: { hub: hubTargetFromOption(choice) } })
              }
            >
              {choice === 'none' ? 'Remove hub' : 'Set hub'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      {verificationDialog}
    </div>
  )
}
