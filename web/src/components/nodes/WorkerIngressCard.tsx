// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ChevronRight, Globe, Loader2 } from 'lucide-react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import {
  adminGetNodeOptions,
  adminListNodesOptions,
  adminSetNodePublicIngressMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { NodeInfoResponse } from '@/api/client/types.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { workerIngressStatus } from '@/lib/worker-ingress'
import { problemDetail } from '@/lib/api-problem'

export function WorkerIngressCard({ node }: { node: NodeInfoResponse }) {
  const queryClient = useQueryClient()
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const update = useMutation({
    ...adminSetNodePublicIngressMutation(),
    onSuccess: async () => {
      toast.success('Worker ingress configuration saved')
      await Promise.all([
        queryClient.invalidateQueries({
          queryKey: adminGetNodeOptions({ path: { node_id: node.id } })
            .queryKey,
        }),
        queryClient.invalidateQueries({
          queryKey: adminListNodesOptions().queryKey,
        }),
      ])
    },
    onError: (error, variables) => {
      if (handleSensitiveActionError(error, () => update.mutate(variables)))
        return
      toast.error('Could not update worker ingress', {
        description: problemDetail(
          error,
          'Check your permissions and try again.'
        ),
      })
    },
  })
  const state = workerIngressStatus(node)

  return (
    <Card className="shadow-none" id="public-ingress">
      <CardContent className="space-y-3 p-4">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="min-w-0 space-y-1">
            <div className="flex items-center gap-2 text-sm font-medium">
              <Globe className="h-4 w-4" /> Public ingress
              <Badge variant="outline">{state.label}</Badge>
            </div>
            <p className="text-sm text-muted-foreground">
              Receive application traffic on this worker and forward it to
              containers on any reachable worker in the cluster.
            </p>
            <p className="text-sm" role="status">
              {state.description}
            </p>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <Button variant="link" asChild className="px-0">
              <Link to="/domains">Domains and certificates</Link>
            </Button>
            <Button
              size="sm"
              variant={node.public_ingress_enabled ? 'outline' : 'default'}
              disabled={update.isPending}
              onClick={() =>
                update.mutate({
                  path: { node_id: node.id },
                  body: { enabled: !node.public_ingress_enabled },
                })
              }
            >
              {update.isPending && (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              )}
              {node.public_ingress_enabled
                ? 'Disable public ingress'
                : 'Enable public ingress'}
            </Button>
          </div>
        </div>
        {node.public_ingress_enabled &&
          ((node.public_ingress_unsupported_route_count ?? 0) > 0 ||
            node.public_ingress_unsupported_reasons.length > 0) && (
            <Alert>
              <AlertDescription className="space-y-2">
                <p>
                  {node.public_ingress_unsupported_route_count ?? 0} application
                  routes are unavailable through this worker.
                </p>
                {node.public_ingress_unsupported_reasons.length > 0 && (
                  <ul className="list-disc space-y-1 pl-5">
                    {node.public_ingress_unsupported_reasons.map((reason) => (
                      <li key={reason}>{reason}</li>
                    ))}
                  </ul>
                )}
              </AlertDescription>
            </Alert>
          )}
        {node.public_ingress_enabled && (
          <Alert>
            <AlertDescription>
              Before disabling ingress, move DNS or load-balancer traffic to
              another ingress worker. Disabling it stops new application traffic
              entering through this worker.
            </AlertDescription>
          </Alert>
        )}
        <Collapsible>
          <CollapsibleTrigger className="group flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground">
            <ChevronRight className="h-4 w-4 transition-transform group-data-[state=open]:rotate-90" />
            Setup guide and limits
          </CollapsibleTrigger>
          <CollapsibleContent className="space-y-3 pt-3">
            <ol className="list-decimal space-y-2 pl-5 text-sm text-muted-foreground">
              <li>
                For an existing worker, upgrade Temps and rerun its original{' '}
                <code>temps join</code> command with the same node name and
                control plane. Keep its saved agent configuration: the saved
                credentials preserve the node identity while enrolling its
                ingress key.
              </li>
              <li>
                Configure your worker’s existing Temps agent service with{' '}
                <code className="break-all">
                  --public-ingress-address &lt;interface-ip&gt;
                </code>{' '}
                and restart that service. Use an IP assigned to its network
                interface, even when its public IP is provided through NAT.
              </li>
              <li>
                Allow inbound HTTP and HTTPS on this worker and verify its
                private connection to the other workers.
              </li>
              <li>
                Enable ingress and wait for the worker to report its listeners.
              </li>
              <li>
                Point your domain’s A record to this worker’s public IPv4
                address. Add an AAAA record only when public IPv6 is configured.
              </li>
              <li>
                Configure the domain and its certificate, then verify HTTPS
                before switching production traffic.
              </li>
            </ol>
            <ul className="list-disc space-y-1 pl-5 text-xs text-muted-foreground">
              <li>
                Supports container applications with domain-specific TLS
                certificates. Wildcard-only certificates are not distributed to
                workers. Routes that require static-file serving, redirects,
                wake-up, or security policies that workers cannot enforce stay
                unavailable through worker ingress.
              </li>
              <li>
                Use the public IP assigned by your infrastructure provider, not
                the worker’s private cluster address. A single ingress worker is
                a single entry point; use a health-checking load balancer for
                ingress failover.
              </li>
              <li>
                Workers retain their last authorized routing configuration for
                up to five minutes without the control plane. After that, new
                requests fail closed until synchronization recovers.
              </li>
            </ul>
          </CollapsibleContent>
        </Collapsible>
      </CardContent>
      {verificationDialog}
    </Card>
  )
}
