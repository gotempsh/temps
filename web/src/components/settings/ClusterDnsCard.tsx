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
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { useSettings, useUpdateSettings } from '@/hooks/useSettings'
import { AlertTriangle, Globe } from 'lucide-react'
import { toast } from 'sonner'

/**
 * Cluster DNS (ADR-024, experimental beta) toggle.
 *
 * Off by default — a past incident showed the injected resolver adding
 * 22-27s delays to outbound connections when it was slow to answer for
 * non-`*.temps.local` hostnames (glibc cycled through all three configured
 * nameservers at ~5s timeout x2 attempts each before falling through).
 * Operators who need `*.temps.local` service-to-service resolution inside
 * containers (e.g. reaching a managed Postgres cluster by hostname from a
 * single control-plane node, where no worker agent is running its own
 * resolver) must explicitly opt in here.
 */
export function ClusterDnsCard() {
  const { data: settings, isLoading, error } = useSettings()
  const updateSettings = useUpdateSettings()

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
          <AlertTitle>Known risk before enabling</AlertTitle>
          <AlertDescription>
            A past incident showed 22-27s outbound connection delays when the
            injected resolver was slow to answer for hostnames outside{' '}
            <code>*.temps.local</code> — glibc retried all three configured
            nameservers at ~5s timeout x2 attempts each. Only enable this if you
            need <code>*.temps.local</code> resolution inside containers.
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
      </CardContent>
    </Card>
  )
}
