// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Label } from '@/components/ui/label'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { CopyButton } from '@/components/ui/copy-button'
import { useSettings, useUpdateSettings } from '@/hooks/useSettings'
import { buildMultiNodeSetupCommand } from '@/lib/cluster-network-command'
import { AlertTriangle, Globe, LockKeyhole, Network } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'

/**
 * Cluster DNS (ADR-024, experimental beta) toggle.
 *
 * Off by default. Operators who need `*.temps.local` service-to-service
 * resolution inside containers must explicitly opt in here.
 */
export function ClusterDnsCard() {
  const { data: settings, isLoading, error } = useSettings()
  const updateSettings = useUpdateSettings()
  const clusterNetwork = settings?.multi_node.cluster_network

  if (isLoading) {
    return (
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Globe className="h-5 w-5" />
            Cluster DNS
          </CardTitle>
        </CardHeader>
        <CardContent>
          <Skeleton className="h-16 w-full" />
        </CardContent>
      </Card>
    )
  }

  if (error || !settings) {
    return (
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Globe className="h-5 w-5" />
            Cluster DNS
          </CardTitle>
        </CardHeader>
        <CardContent>
          <Alert variant="destructive">
            <AlertTriangle className="h-4 w-4" />
            <AlertTitle>Failed to load settings</AlertTitle>
            <AlertDescription>
              The server returned an error. Check console logs or contact your
              administrator.
            </AlertDescription>
          </Alert>
        </CardContent>
      </Card>
    )
  }

  const enabled = settings.cluster_dns?.enabled ?? false

  const onCheckedChange = (checked: boolean) => {
    updateSettings.mutate(
      { cluster_dns: { enabled: checked } },
      {
        onSuccess: () =>
          toast.success(
            checked ? 'Cluster DNS enabled' : 'Cluster DNS disabled'
          ),
      }
    )
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Globe className="h-5 w-5" />
          Cluster DNS
          <span className="text-xs font-normal text-muted-foreground">
            (experimental)
          </span>
        </CardTitle>
        <CardDescription>
          Lets deployed containers resolve <code>*.temps.local</code> hostnames
          — required for service-to-service traffic such as reaching a managed
          Postgres cluster by hostname (e.g.{' '}
          <code>primary.pg-orders.temps.local</code>) from a single
          control-plane node with no worker agent of its own.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <Alert>
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>Cluster-wide setting</AlertTitle>
          <AlertDescription>
            Incorrect DNS configuration can break service discovery across the
            cluster. Verify node and application health before and after
            changing it.
          </AlertDescription>
        </Alert>

        <div className="flex items-start justify-between rounded-lg border p-3">
          <div className="space-y-0.5">
            <Label htmlFor="cluster-dns-enabled" className="text-sm">
              Enable cluster DNS
            </Label>
            <p className="text-xs text-muted-foreground max-w-prose">
              Starts the control-plane Hickory resolver and injects it as the
              first nameserver into every deployed container.
            </p>
          </div>
          <Switch
            id="cluster-dns-enabled"
            checked={enabled}
            disabled={updateSettings.isPending}
            onCheckedChange={onCheckedChange}
          />
        </div>

        <div className="rounded-lg border">
          <div className="flex items-start gap-3 border-b p-4">
            <Network className="mt-0.5 h-5 w-5 text-muted-foreground" />
            <div className="space-y-1">
              <p className="text-sm font-medium">Container network</p>
              <p className="text-xs text-muted-foreground">
                Temps assigns one subnet from this cluster-wide pool to each
                control-plane or worker node.
              </p>
            </div>
          </div>

          {clusterNetwork ? (
            <ClusterNetworkConfiguration
              key={`${clusterNetwork.compute_pool_cidr}/${clusterNetwork.subnet_prefix_len}`}
              computePoolCidr={clusterNetwork.compute_pool_cidr}
              subnetPrefixLen={clusterNetwork.subnet_prefix_len}
              allocationCount={clusterNetwork.allocation_count}
              locked={clusterNetwork.locked}
            />
          ) : (
            <p className="p-4 text-xs text-muted-foreground">
              Cluster network state is unavailable. Run{' '}
              <code>temps network status</code> on the control-plane host to
              inspect it.
            </p>
          )}
        </div>
      </CardContent>
    </Card>
  )
}

interface ClusterNetworkConfigurationProps {
  computePoolCidr: string
  subnetPrefixLen: number
  allocationCount: number
  locked: boolean
}

function ClusterNetworkConfiguration({
  computePoolCidr,
  subnetPrefixLen,
  allocationCount,
  locked,
}: ClusterNetworkConfigurationProps) {
  const [poolCidr, setPoolCidr] = useState(computePoolCidr)
  const [nodePrefix, setNodePrefix] = useState(String(subnetPrefixLen))
  const setupCommand = buildMultiNodeSetupCommand(poolCidr, nodePrefix)

  return (
    <div className="space-y-4 p-4">
      <div className="grid gap-4 sm:grid-cols-2">
        <div className="space-y-2">
          <Label htmlFor="cluster-compute-pool">Pool CIDR</Label>
          <Input
            id="cluster-compute-pool"
            value={poolCidr}
            disabled={locked}
            onChange={(event) => setPoolCidr(event.target.value)}
            spellCheck={false}
            className="font-mono"
          />
        </div>
        <div className="space-y-2">
          <Label htmlFor="cluster-node-prefix">Per-node prefix</Label>
          <Input
            id="cluster-node-prefix"
            value={nodePrefix}
            disabled={locked}
            onChange={(event) => setNodePrefix(event.target.value)}
            inputMode="numeric"
            className="font-mono"
          />
        </div>
      </div>

      {locked ? (
        <div className="flex gap-2 rounded-md bg-muted/50 p-3 text-xs text-muted-foreground">
          <LockKeyhole className="h-4 w-4 shrink-0" />
          <p>
            Locked after {allocationCount}{' '}
            {allocationCount === 1
              ? 'network allocation'
              : 'network allocations'}
            . Changing an active pool requires an explicit cluster network
            migration so existing workloads are not stranded.
          </p>
        </div>
      ) : (
        <div className="space-y-2">
          <p className="text-xs text-muted-foreground">
            No subnets have been allocated yet. Run this command on the
            control-plane host to set the pool safely:
          </p>
          <div className="flex items-center gap-2 rounded-md bg-muted p-3">
            <code className="min-w-0 flex-1 overflow-x-auto text-xs">
              {setupCommand}
            </code>
            <CopyButton
              minimal
              value={setupCommand}
              label="Copy network setup command"
            />
          </div>
        </div>
      )}
    </div>
  )
}
