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
import {
  getEnvironmentIpAccess,
  getProjectIpAccess,
  putEnvironmentIpAccess,
  putProjectIpAccess,
  type UptimeMonitorIpBlock,
} from '@/api/client'
import {
  getCurrentMonitorStatusQueryKey,
  getEnvironmentIpAccessQueryKey,
  getProjectIpAccessQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { problemDetail } from '@/lib/api-problem'
import { queryKeyPrefix } from '@/lib/refresh'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Callout } from '@temps-sdk/ds'
import { Loader2 } from 'lucide-react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import {
  monitorBlockSettingsPath,
  withMonitorEntries,
} from './settings/ip-access'

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
  const projectId = block.project_id
  const environmentId =
    block.policy_scope === 'environment' ? block.environment_id : null

  const allow = useMutation({
    mutationFn: async () => {
      // Read the list right before replacing it, so every entry someone
      // else saved meanwhile is kept.
      const current =
        environmentId != null
          ? await getEnvironmentIpAccess({
              path: { project_id: projectId, environment_id: environmentId },
              throwOnError: true,
            })
          : await getProjectIpAccess({
              path: { project_id: projectId },
              throwOnError: true,
            })
      const body = {
        mode: current.data.mode,
        cidrs: withMonitorEntries(current.data.cidrs, block).map((entry) => ({
          cidr: entry.cidr,
          description: entry.description ?? null,
        })),
        acknowledge_lockout: false,
      }
      if (environmentId != null) {
        await putEnvironmentIpAccess({
          path: { project_id: projectId, environment_id: environmentId },
          body,
          throwOnError: true,
        })
      } else {
        await putProjectIpAccess({
          path: { project_id: projectId },
          body,
          throwOnError: true,
        })
      }
    },
    onSuccess: () => {
      toast.success('Monitor allowed. The next check will reach the app.')
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
    },
    onError: (error) => {
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
            onClick={() => allow.mutate()}
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
    </Callout>
  )
}
