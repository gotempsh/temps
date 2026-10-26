// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { MouseEvent } from 'react'
import { Loader2, Trash2 } from 'lucide-react'
import type { NodeInfoResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { useNodeRemoval } from '@/components/nodes/useNodeRemoval'
import {
  nodeRemoval,
  nodeRemovalBlocker,
  type NodeRemoval,
} from '@/lib/worker-nodes'

/**
 * Clicks inside the row's actions must not reach the row, which navigates to
 * the node. React bubbles events from a portal through its React parents, so
 * this also covers the confirmation dialog rendered in a portal below.
 */
function stopRowNavigation(event: MouseEvent) {
  event.stopPropagation()
}

function RemovalBlocker({ removal }: { removal: NodeRemoval }) {
  const blocker = nodeRemovalBlocker(removal)
  if (!blocker) return null
  // Clicking it falls through to the row, which opens the node page where
  // the drain (or the sandboxes) can be dealt with.
  return (
    <span className="block text-right text-xs text-muted-foreground underline-offset-2 hover:underline">
      {blocker}
    </span>
  )
}

/**
 * The trailing cell of a worker-node row: a one-step Remove when nothing
 * stands in the way, otherwise what does. Each row owns its removal flow,
 * and its dialog stays mounted while the row exists, so a refusal it is
 * showing survives the list refetching underneath it.
 */
export function NodeRowActions({
  node,
  now,
}: {
  node: NodeInfoResponse
  /** Pinned in tests; defaults to the current time. */
  now?: number
}) {
  const { requestRemoval, pending, dialog } = useNodeRemoval({
    nodeId: node.id,
    nodeName: node.name,
  })
  const removal = nodeRemoval(node, now)
  return (
    <div className="flex justify-end">
      <RemovalBlocker removal={removal} />
      <div className="contents" onClick={stopRowNavigation}>
        {removal.kind === 'remove' && (
          <Button
            variant="ghost"
            size="sm"
            className="h-7 gap-1.5 text-destructive hover:text-destructive"
            onClick={requestRemoval}
            disabled={pending}
            aria-label={`Remove node ${node.name}`}
          >
            {pending ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <Trash2 className="h-4 w-4" />
            )}
            <span className="max-sm:sr-only">Remove</span>
          </Button>
        )}
        {dialog}
      </div>
    </div>
  )
}
