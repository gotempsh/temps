// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Shown on an uptime monitor when the project's own IP access list refuses
 * Temps' managed monitor, so every check sees a 403 instead of the app.
 *
 * The fix is offered, never applied on the operator's behalf: "Allow monitor"
 * adds the addresses the monitor connects from to the list that refuses it,
 * keeping every other entry, and the settings link opens that list.
 */
import { type UptimeMonitorIpBlock } from '@/api/client'
import {
  getCurrentMonitorStatusQueryKey,
  getEnvironmentIpAccessQueryKey,
  getProjectIpAccessQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
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
import { Button } from '@/components/ui/button'
import { problemDetail } from '@/lib/api-problem'
import { queryKeyPrefix } from '@/lib/refresh'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Callout } from '@temps-sdk/ds'
import { Loader2 } from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import {
  allowMonitor,
  allowMonitorNotice,
  allowMonitorTarget,
} from './settings/allow-monitor'
import { isLockoutError, monitorBlockSettingsPath } from './settings/ip-access'

interface MonitorIpAccessWarningProps {
  block: UptimeMonitorIpBlock
  monitorId: number
  projectSlug: string
}

export function MonitorIpAccessWarning({
  block,
  monitorId,
  projectSlug,
}: MonitorIpAccessWarningProps) {
  const queryClient = useQueryClient()
  const [lockoutDetail, setLockoutDetail] = useState<string | null>(null)

  const refresh = () => {
    const { projectId, environmentId } = allowMonitorTarget(block)
    queryClient.invalidateQueries({
      queryKey:
        environmentId != null
          ? getEnvironmentIpAccessQueryKey({
              path: { project_id: projectId, environment_id: environmentId },
            })
          : getProjectIpAccessQueryKey({ path: { project_id: projectId } }),
    })
    queryClient.invalidateQueries({
      queryKey: queryKeyPrefix(getCurrentMonitorStatusQueryKey, {
        path: { monitor_id: monitorId },
      }),
    })
  }

  const allow = useMutation({
    mutationFn: (acknowledgeLockout: boolean) =>
      allowMonitor(block, acknowledgeLockout),
    onSuccess: (outcome) => {
      setLockoutDetail(null)
      refresh()
      switch (outcome.kind) {
        case 'saved':
          toast.success('Monitor allowed. The next check will reach the app.')
          return
        case 'already-allowed':
          toast.success(allowMonitorNotice(outcome))
          return
        case 'changed':
        case 'full':
          toast.error(allowMonitorNotice(outcome))
          return
      }
    },
    onError: (error) => {
      if (isLockoutError(error)) {
        setLockoutDetail(
          problemDetail(error, 'This change would lock you out.')
        )
        return
      }
      toast.error(
        problemDetail(error, 'Could not add the monitor to the IP access list')
      )
    },
  })

  const canAllow = block.suggested_cidrs.length > 0

  return (
    <Callout tone="warning" title="The IP access list blocks this monitor">
      <p className="[overflow-wrap:anywhere]">{block.message}</p>
      <div className="mt-3 flex flex-wrap gap-2">
        {canAllow && (
          <Button
            size="sm"
            onClick={() => allow.mutate(false)}
            disabled={allow.isPending}
            title={`Add ${block.suggested_cidrs.join(', ')} to the list as "${block.suggested_description}"`}
          >
            {allow.isPending && (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            )}
            Allow monitor
          </Button>
        )}
        <Button asChild size="sm" variant="outline">
          <Link to={monitorBlockSettingsPath(projectSlug, block)}>
            IP access
          </Link>
        </Button>
      </div>

      <AlertDialog
        open={lockoutDetail !== null}
        onOpenChange={(open) => {
          if (!open) setLockoutDetail(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>This would lock you out</AlertDialogTitle>
            <AlertDialogDescription>{lockoutDetail}</AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction onClick={() => allow.mutate(true)}>
              Save anyway
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </Callout>
  )
}
