// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { useSettings } from '@/hooks/useSettings'
import { CLUSTER_DNS_URL } from '@/lib/worker-nodes'
import { AlertTriangle } from 'lucide-react'
import { Link } from 'react-router'

/**
 * Warns, where a placement depends on it, that Cluster DNS is off. Links to
 * the switch instead of flipping it: the setting is cluster-wide and the
 * operator decides. Renders nothing while it is on, or when the settings
 * cannot be read (a role without settings access cannot change it anyway).
 */
export function ClusterDnsOffNotice({
  title,
  children,
}: {
  title: string
  children: React.ReactNode
}) {
  const { data: settings } = useSettings()
  if (!settings || settings.cluster_dns?.enabled !== false) return null
  return <ClusterDnsOffAlert title={title}>{children}</ClusterDnsOffAlert>
}

export function ClusterDnsOffAlert({
  title,
  children,
}: {
  title: string
  children: React.ReactNode
}) {
  return (
    <Alert className="border-amber-500/30 bg-amber-500/5">
      <AlertTriangle className="h-4 w-4 text-amber-500" />
      <AlertTitle className="text-amber-700 dark:text-amber-400">
        {title}
      </AlertTitle>
      <AlertDescription className="space-y-2 text-amber-700 dark:text-amber-300">
        <p>{children}</p>
        <Button asChild size="sm" variant="outline">
          <Link to={CLUSTER_DNS_URL}>Open Cluster DNS settings</Link>
        </Button>
      </AlertDescription>
    </Alert>
  )
}
