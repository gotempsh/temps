// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getComposePlacementOptions,
  getComposePlacementQueryKey,
  moveComposeStackMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ComposePlacementResponse,
  NodeInfoResponse,
} from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import {
  composeHomeSummary,
  composeMoveCandidates,
  composeMoveLosesData,
  composeRunningNodeId,
} from '@/lib/compose-placement'
import { cn } from '@/lib/utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertTriangle, HardDrive, MoveRight } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'

interface ComposePlacementPanelProps {
  projectId: number
  environmentId: number
  /** Nodes a stack can move to (active nodes, including the control plane). */
  nodes: NodeInfoResponse[]
  /** Whether `nodes` has loaded; moving waits for it. */
  nodesLoaded: boolean
  /** Why the node list could not be loaded, if it could not. */
  nodesError?: Error | null
}

/**
 * Where this environment's Compose stack lives, and the only way to change
 * it. A stack stays on the node its first deployment ran on because its
 * volumes are there; moving it starts it empty on the new node.
 */
export function ComposePlacementPanel({
  projectId,
  environmentId,
  nodes,
  nodesLoaded,
  nodesError,
}: ComposePlacementPanelProps) {
  const queryClient = useQueryClient()
  const [moveOpen, setMoveOpen] = useState(false)
  const placementQuery = useQuery({
    ...getComposePlacementOptions({
      path: { project_id: projectId, environment_id: environmentId },
    }),
  })

  if (placementQuery.isPending) {
    return <Skeleton className="h-20 w-full" />
  }
  if (placementQuery.isError) {
    return (
      <Alert variant="destructive">
        <AlertTriangle className="h-4 w-4" aria-hidden="true" />
        <AlertTitle>Could not load where this Compose stack runs</AlertTitle>
        <AlertDescription>{placementQuery.error.toString()}</AlertDescription>
      </Alert>
    )
  }

  const placement = placementQuery.data
  const summary = composeHomeSummary(placement)
  const homeOffline =
    placement.pinned &&
    placement.home_node_status != null &&
    placement.home_node_status !== 'active'
  const movePending = placement.moving_from_node_id != null

  return (
    <div className="space-y-2 rounded-lg border p-3">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
        <div className="flex min-w-0 gap-2">
          <HardDrive
            className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground"
            aria-hidden="true"
          />
          <div className="min-w-0">
            <p className="text-sm font-medium">{summary.title}</p>
            <p className="text-xs text-muted-foreground">{summary.detail}</p>
          </div>
        </div>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={!nodesLoaded}
          onClick={() => setMoveOpen(true)}
        >
          <MoveRight className="h-4 w-4" aria-hidden="true" />
          Move stack
        </Button>
      </div>

      {nodesError && (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" aria-hidden="true" />
          <AlertTitle>Could not load the cluster&apos;s nodes</AlertTitle>
          <AlertDescription>
            The stack&apos;s placement is shown, but it cannot be moved until
            the node list loads: {nodesError.toString()}
          </AlertDescription>
        </Alert>
      )}
      {movePending && (
        <Alert variant="warning">
          <AlertTriangle className="h-4 w-4" aria-hidden="true" />
          <AlertTitle>Move pending: redeploy to complete it</AlertTitle>
          <AlertDescription>
            The next deployment stops the stack on{' '}
            {placement.moving_from_node_name} and starts it on{' '}
            {placement.home_node_name} with empty volumes. The data on{' '}
            {placement.moving_from_node_name} stays there.
          </AlertDescription>
        </Alert>
      )}
      {homeOffline && (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" aria-hidden="true" />
          <AlertTitle>
            {placement.home_node_name} is {placement.home_node_status}
          </AlertTitle>
          <AlertDescription>
            {homeOfflineExplanation(placement, movePending)}
          </AlertDescription>
        </Alert>
      )}
      {placement.drift && (
        <Alert variant="warning">
          <AlertTriangle className="h-4 w-4" aria-hidden="true" />
          <AlertTitle>Target Nodes no longer match this stack</AlertTitle>
          <AlertDescription>{placement.drift}</AlertDescription>
        </Alert>
      )}

      <MoveComposeStackDialog
        open={moveOpen}
        onOpenChange={setMoveOpen}
        placement={placement}
        nodes={nodes}
        onMoved={(updated) => {
          queryClient.setQueryData(
            getComposePlacementQueryKey({
              path: { project_id: projectId, environment_id: environmentId },
            }),
            updated
          )
        }}
      />
    </div>
  )
}

/**
 * Why an unavailable home blocks deployments. During a pending move the home
 * is the destination, which holds no data yet; the volumes are still on the
 * node the move is leaving.
 */
function homeOfflineExplanation(
  placement: ComposePlacementResponse,
  movePending: boolean
): string {
  if (movePending) {
    return `The next deployment cannot start the stack there, so it keeps running on ${placement.moving_from_node_name} with its volumes. Bring ${placement.home_node_name} back online, or move the stack back to ${placement.moving_from_node_name} to cancel the move and keep its data.`
  }
  return 'Deployments of this stack fail until it is back, because its volumes are on it. Temps never moves a Compose stack on its own; bring the node back online, or move the stack (its data stays behind).'
}

interface MoveComposeStackDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  placement: ComposePlacementResponse
  nodes: NodeInfoResponse[]
  onMoved: (placement: ComposePlacementResponse) => void
}

function MoveComposeStackDialog({
  open,
  onOpenChange,
  placement,
  nodes,
  onMoved,
}: MoveComposeStackDialogProps) {
  const [target, setTarget] = useState<number | null>(null)
  const [acknowledged, setAcknowledged] = useState(false)
  const move = useMutation({
    ...moveComposeStackMutation(),
    meta: { errorTitle: 'Failed to move the Compose stack' },
    onSuccess: (updated) => {
      onMoved(updated)
      toast.success(
        placement.moving_from_node_id != null &&
          updated.moving_from_node_id == null
          ? `Move cancelled: the stack stays on ${updated.home_node_name} with its data`
          : `Stack will move to ${updated.home_node_name} on the next deployment`
      )
      close()
    },
  })

  const candidates = composeMoveCandidates(placement, nodes)
  const runningNodeId = composeRunningNodeId(placement)
  const runningNodeName =
    placement.moving_from_node_id != null
      ? placement.moving_from_node_name
      : placement.home_node_name
  const cancelsMove =
    placement.moving_from_node_id != null && target === runningNodeId
  const needsAcknowledgement =
    target != null && composeMoveLosesData(placement, target)
  const canMove =
    target != null && (!needsAcknowledgement || acknowledged) && !move.isPending

  function close() {
    setTarget(null)
    setAcknowledged(false)
    onOpenChange(false)
  }

  return (
    <Dialog open={open} onOpenChange={(next) => (next ? null : close())}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Move this Compose stack</DialogTitle>
          <DialogDescription>
            {composeMoveDescription(placement, runningNodeName)}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-2" role="radiogroup" aria-label="Target node">
          {candidates.length === 0 && (
            <p className="text-sm text-muted-foreground">
              No other active node can take this stack.
            </p>
          )}
          {candidates.map((node) => (
            <button
              key={node.id}
              type="button"
              role="radio"
              aria-checked={target === node.id}
              onClick={() => setTarget(node.id)}
              className={cn(
                'flex w-full items-center justify-between gap-3 rounded-lg border p-2 text-left hover:bg-muted/50',
                target === node.id && 'border-primary bg-muted/50'
              )}
            >
              <span className="text-sm font-medium">{node.name}</span>
              <span className="text-xs text-muted-foreground">
                {node.private_address}
              </span>
            </button>
          ))}
        </div>

        {needsAcknowledgement && (
          <div className="flex items-start gap-2 rounded-lg border border-warning/50 bg-warning/10 p-3">
            <Checkbox
              id="compose-move-acknowledge"
              checked={acknowledged}
              onCheckedChange={(checked) => setAcknowledged(checked === true)}
            />
            <Label
              htmlFor="compose-move-acknowledge"
              className="text-sm font-normal leading-snug"
            >
              I understand the stack starts with empty volumes on the new node.
              Its data stays on {runningNodeName} and is not copied.
            </Label>
          </div>
        )}

        <DialogFooter>
          <Button type="button" variant="outline" onClick={close}>
            Cancel
          </Button>
          <Button
            type="button"
            variant={needsAcknowledgement ? 'destructive' : 'default'}
            disabled={!canMove}
            onClick={() => {
              if (target == null) return
              move.mutate({
                path: {
                  project_id: placement.project_id,
                  environment_id: placement.environment_id,
                },
                body: {
                  node_id: target,
                  acknowledge_data_loss: acknowledged,
                },
              })
            }}
          >
            {moveButtonLabel(move.isPending, cancelsMove)}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function composeMoveDescription(
  placement: ComposePlacementResponse,
  runningNodeName: string | null | undefined
): string {
  if (!placement.pinned) {
    return 'It has not run yet. Choose where its first deployment goes.'
  }
  if (placement.moving_from_node_id != null) {
    return `It still runs on ${runningNodeName}, with its volumes, until the next deployment moves it to ${placement.home_node_name}. Choose ${runningNodeName} to cancel the move and keep its data, or another node to start it there with empty volumes.`
  }
  return `It runs on ${placement.home_node_name} and keeps its volumes there. On the next deployment Temps stops it there and starts it on the node you choose with empty volumes.`
}

function moveButtonLabel(pending: boolean, cancelsMove: boolean): string {
  if (pending) return cancelsMove ? 'Cancelling…' : 'Moving…'
  return cancelsMove ? 'Cancel move' : 'Move stack'
}
