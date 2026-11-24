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
import { ClusterDnsCoverageSection } from './ClusterDnsCoverage'
import { toast } from 'sonner'

/**
 * Cluster DNS (ADR-024, experimental beta) toggle.
 *
 * Off on a single host; turned on automatically when the first worker joins,
 * unless an operator has already chosen here. Switching it takes effect
 * without a restart, for containers created from then on; the applications
 * still running without it are listed with a redeploy action.
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
          Lets applications reach other services by their{' '}
          <code>*.temps.local</code> names, on any node — for example a managed
          Postgres cluster at <code>primary.pg-orders.temps.local</code>,
          wherever its members run.
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
              Starts the resolver on every node and makes it the first
              nameserver of each application container created from now on. No
              restart is needed. Running applications keep their nameservers
              until redeployed; databases do not need it. Turned on
              automatically when the first worker joins, unless you have already
              chosen here.
            </p>
          </div>
          <Switch
            id="cluster-dns-enabled"
            checked={enabled}
            disabled={updateSettings.isPending}
            onCheckedChange={onCheckedChange}
          />
        </div>

        <ClusterDnsCoverageSection />
      </CardContent>
    </Card>
  )
}
