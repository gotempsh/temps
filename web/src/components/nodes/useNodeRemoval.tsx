// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { adminListNodesOptions } from '@/api/client/@tanstack/react-query.gen'
import { client } from '@/api/client/client.gen'
import type {
  NodePlacementReference,
  RemoveNodeResponse,
} from '@/api/client/types.gen'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { NODE_HOSTS_SANDBOXES_TYPE } from '@/components/nodes/node-eviction'
import { useInvalidateNodeCapability } from '@/hooks/useNodeCapability'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { problemDetail } from '@/lib/api-problem'
import { describeUpdatedPlacements } from '@/lib/node-placements'
import {
  servingOnUnreachableNode,
  type ServingOnUnreachableNode,
  unremovedNodeContainers,
  type UnremovedNodeContainers,
} from '@/lib/worker-nodes'

/**
 * What removing a node affects. Plain inline content only: the dialog wraps it
 * in a paragraph, so the refusal lists are joined into the sentence.
 */
export function removeNodeConsequences(
  pinnedPlacements: NodePlacementReference[] | null,
  unremovedContainers: UnremovedNodeContainers | null,
  servingOnGoneNode: ServingOnUnreachableNode | null
): ReactNode {
  if (pinnedPlacements) {
    const targets = pinnedPlacements
      .map((placement) =>
        placement.environment_name
          ? `${placement.project_name} / ${placement.environment_name}`
          : `${placement.project_name} (project default)`
      )
      .join(', ')
    return `The node was not removed: it is the only target node of ${targets}. Removing it would leave them with no target, so they could run on any node. Change their target nodes in each environment’s settings, then remove the node.`
  }
  if (servingOnGoneNode) {
    return (
      <>
        This node stopped sending heartbeats, so a drain cannot move the{' '}
        {servingOnGoneNode.count} container(s) live deployments still have on it
        {servingOnGoneNode.environments.length > 0 && (
          <>
            {' '}
            (
            <span className="break-words">
              {servingOnGoneNode.environments.join(', ')}
            </span>
            )
          </>
        )}
        . If the host still exists, start its agent and drain it instead. If it
        is gone for good, remove it anyway: those containers are recorded as
        orphaned, and any of these environments with no replicas on another node
        stops serving until you redeploy it.
      </>
    )
  }
  if (unremovedContainers) {
    return (
      <>
        Temps could not confirm that {unremovedContainers.count} container(s) it
        placed on this node are gone, so the node was not removed:{' '}
        <span className="font-mono text-xs break-all">
          {unremovedContainers.lines.join(', ')}
        </span>
        . Start the agent on the node and try again, or remove them on that host
        with docker rm -f. If the host is gone for good, remove the node anyway:
        these containers are then recorded as orphaned and never touched again.
      </>
    )
  }
  return 'Temps removes the containers it left on the node and takes it out of the target nodes of every environment that lists it. The node must host no sandboxes and run no deployment containers; drain it first if it still does.'
}

export interface UseNodeRemovalOptions {
  nodeId: number
  nodeName: string
  /** Called after the node is removed (e.g. to leave its detail page). */
  onRemoved?: () => void
  /**
   * Offered as the action of the "hosts sandboxes" refusal toast. Without it,
   * the toast still explains the refusal but offers no shortcut.
   */
  onShowSandboxes?: () => void
}

export interface NodeRemovalFlow {
  /** Open the confirmation dialog. */
  requestRemoval: () => void
  /** A removal request is in flight. */
  pending: boolean
  /**
   * The confirmation and step-up verification dialogs. Always render it: the
   * dialogs are controlled by `open`, never mounted conditionally.
   */
  dialog: ReactNode
}

/**
 * The whole "remove a node" flow: the confirmation dialog, the refusals the
 * API answers with (pinned placements, an unreachable node still serving,
 * containers it could not confirm are gone, sandboxes), the forced retry, the
 * step-up re-verification and the success toasts.
 */
export function useNodeRemoval({
  nodeId,
  nodeName,
  onRemoved,
  onShowSandboxes,
}: UseNodeRemovalOptions): NodeRemovalFlow {
  const queryClient = useQueryClient()
  // Removing a node changes what this installation can schedule, so the
  // capability every page's banner reads is stale the moment it succeeds.
  const invalidateCapability = useInvalidateNodeCapability()
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const [open, setOpen] = useState(false)
  const [pending, setPending] = useState(false)
  // Containers a removal could not confirm are gone. While set, the dialog
  // lists them and offers to remove the node anyway.
  const [unremovedContainers, setUnremovedContainers] =
    useState<UnremovedNodeContainers | null>(null)
  // Environments pinned only to this node, from a refused removal.
  const [pinnedPlacements, setPinnedPlacements] = useState<
    NodePlacementReference[] | null
  >(null)
  // Live deployments an unreachable node still serves, from a refused
  // removal. While set, the dialog names them and offers to remove anyway.
  const [servingOnGoneNode, setServingOnGoneNode] =
    useState<ServingOnUnreachableNode | null>(null)

  const resetRefusals = () => {
    setUnremovedContainers(null)
    setPinnedPlacements(null)
    setServingOnGoneNode(null)
  }

  const handleRemove = async (force = false) => {
    setPending(true)
    let keepDialogOpen = false
    try {
      const resp = await client.delete({
        url: '/internal/nodes/{node_id}',
        path: { node_id: nodeId },
        query: force ? { force: true } : undefined,
      })
      if (resp.error) {
        if (handleSensitiveActionError(resp.error, () => handleRemove(force))) {
          return
        }
        // Containers Temps placed on the node may still exist: keep the
        // dialog open with the list so the operator decides whether the
        // host is gone for good.
        const pinned = (resp.error as { placements?: unknown }).placements
        if (Array.isArray(pinned) && pinned.length > 0) {
          setPinnedPlacements(pinned as NodePlacementReference[])
          keepDialogOpen = true
          setOpen(true)
          return
        }
        const serving = servingOnUnreachableNode(resp.error)
        if (serving && !force) {
          setServingOnGoneNode(serving)
          keepDialogOpen = true
          setOpen(true)
          return
        }
        const unremoved = unremovedNodeContainers(resp.error)
        if (unremoved && !force) {
          setUnremovedContainers(unremoved)
          keepDialogOpen = true
          setOpen(true)
          return
        }
        // Live sandboxes block removal and draining doesn't move them:
        // point straight at the place that can destroy them.
        const hostsSandboxes =
          (resp.error as { type?: unknown }).type === NODE_HOSTS_SANDBOXES_TYPE
        toast.error('Could not remove node', {
          description: problemDetail(
            resp.error,
            'Check your permissions and try again.'
          ),
          action:
            hostsSandboxes && onShowSandboxes
              ? { label: 'Show sandboxes', onClick: onShowSandboxes }
              : undefined,
        })
        return
      }
      const removal = resp.data as RemoveNodeResponse | undefined
      const orphaned = removal?.orphaned_containers ?? []
      const lost = removal?.lost_environments ?? []
      // Environments whose target nodes listed this node no longer do; say
      // which, since a rejoined machine gets a new ID and is not targeted.
      const placements = describeUpdatedPlacements(removal?.placements_updated)
      if (lost.length > 0) {
        toast.warning('Node removed', {
          description: [
            `These environments were still serving from it; redeploy any with no replicas on another node: ${lost.join(', ')}.`,
            'If the host comes back, remove its containers there with docker rm -f.',
            placements,
          ]
            .filter(Boolean)
            .join(' '),
        })
      } else if (orphaned.length > 0) {
        toast.warning('Node removed', {
          description: [
            'Some containers could not be removed and were recorded as orphaned. If the host still exists, remove them there with docker rm -f.',
            placements,
          ]
            .filter(Boolean)
            .join(' '),
        })
      } else if (placements) {
        toast.warning('Node removed', { description: placements })
      } else {
        toast.success('Node removed')
      }
      queryClient.invalidateQueries({
        queryKey: adminListNodesOptions().queryKey,
      })
      invalidateCapability()
      onRemoved?.()
    } catch {
      toast.error('Failed to remove node')
    } finally {
      setPending(false)
      if (!keepDialogOpen) {
        setOpen(false)
        resetRefusals()
      }
    }
  }

  const forceable = unremovedContainers !== null || servingOnGoneNode !== null

  const dialog = (
    <>
      {verificationDialog}
      <ConfirmDeleteDialog
        open={open}
        onOpenChange={(next) => {
          setOpen(next)
          if (!next) resetRefusals()
        }}
        resourceType="node"
        resourceName={nodeName}
        verb="Remove"
        confirmLabel={forceable ? 'Remove anyway' : 'Remove node'}
        consequences={removeNodeConsequences(
          pinnedPlacements,
          unremovedContainers,
          servingOnGoneNode
        )}
        isPending={pending}
        onConfirm={() => void handleRemove(forceable)}
      />
    </>
  )

  return { requestRemoval: () => setOpen(true), pending, dialog }
}
