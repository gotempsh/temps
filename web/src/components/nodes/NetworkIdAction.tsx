// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  clusterNetworkGetOptions,
  clusterNetworkIdChangeMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'

import { Button } from '@/components/ui/button'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { problemDetail } from '@/lib/api-problem'
import {
  networkIdAction,
  networkIdChangedMessage,
  type ClusterNetworkStatus,
} from '@/lib/cluster-network'

/** The "give this cluster its own network ID" change, with its step-up. */
function useNetworkIdChange() {
  const queryClient = useQueryClient()
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const change = useMutation({
    ...clusterNetworkIdChangeMutation(),
    onSuccess: (answer) => {
      queryClient.setQueryData(
        clusterNetworkGetOptions().queryKey,
        answer.status
      )
      toast.success('This cluster has its own network ID', {
        description: networkIdChangedMessage(answer.change),
      })
    },
    onError: (failure, variables) => {
      if (handleSensitiveActionError(failure, () => change.mutate(variables)))
        return
      toast.error('Could not give this cluster its own network ID', {
        description: problemDetail(
          failure,
          'Nothing was changed; retry, and check the server logs if it persists.'
        ),
      })
    },
  })
  return { change, verificationDialog }
}

/** The button when the change is allowed, else why it is not. */
function NetworkIdChangeControl({ status }: { status: ClusterNetworkStatus }) {
  const { change, verificationDialog } = useNetworkIdChange()
  const action = networkIdAction(status)
  return (
    <>
      {action.canChange ? (
        <Button
          size="sm"
          variant="outline"
          onClick={() => change.mutate({})}
          disabled={change.isPending}
        >
          {change.isPending
            ? 'Assigning a network ID…'
            : 'Give this cluster its own network ID'}
        </Button>
      ) : (
        <div>
          <p className="font-medium">
            This cluster cannot get its own network ID yet:
          </p>
          <ul className="list-disc space-y-1 pl-4">
            {action.refusedBecause.map((reason) => (
              <li key={reason} className="break-words">
                {reason}
              </li>
            ))}
          </ul>
        </div>
      )}
      {verificationDialog}
    </>
  )
}

/**
 * In the "Cross-node network unavailable" banner (ADR 062): when a node's
 * overlay is down because another VXLAN device on its host holds the
 * cluster's network ID, give the cluster its own instead of asking for a
 * shell on that host. When some node could not follow a new one, say which
 * and why in place of the button.
 */
export function NetworkIdAction({
  status,
}: {
  status: ClusterNetworkStatus | undefined
}) {
  const action = networkIdAction(status)
  if (!status || !action.offered) return null
  return (
    <div className="space-y-2">
      <p>
        {action.blockedNodes.join(', ')}{' '}
        {action.blockedNodes.length === 1 ? 'is' : 'are'} blocked by another
        overlay on the same host using network ID {status.vxlan_network_id}.
        Instead of removing it there, this cluster can move to a network ID of
        its own; cross-node traffic pauses for up to 30 seconds while every node
        follows.
      </p>
      <NetworkIdChangeControl status={status} />
    </div>
  )
}

/**
 * Settings → Worker Nodes → Cluster network (ADR 062): the VXLAN network ID
 * the cluster's overlay uses, always shown, and the way to give the cluster
 * its own when another overlay on a node's host holds it.
 */
export function ClusterNetworkIdSection({
  status,
}: {
  status: ClusterNetworkStatus
}) {
  if (status.vxlan_network_id === 0) return null
  return (
    <div className="space-y-2 border-t pt-4 text-xs text-muted-foreground">
      <p className="text-sm font-medium text-foreground">
        Network ID {status.vxlan_network_id}{' '}
        <span className="font-normal text-muted-foreground">
          (UDP port {status.vxlan_port})
        </span>
      </p>
      <p className="max-w-prose">
        Every node&rsquo;s cross-node network carries this VXLAN network ID. A
        host can run one overlay per network ID, so if a node also hosts another
        cluster&rsquo;s overlay on the same ID, give this cluster its own.
        Cross-node traffic pauses for up to 30 seconds while every node follows.
      </p>
      <NetworkIdChangeControl status={status} />
    </div>
  )
}
