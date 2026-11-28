// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ServicePluginResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  pluginLimitsSummary,
  servicePluginLimitsPath,
} from '@/lib/plugin-services'
import { Loader2, Power } from 'lucide-react'
import { Link } from 'react-router'

/**
 * The memory and CPU a plugin's container is limited to. They are the limits
 * of its service, so they are changed there; a plugin with no memory limit
 * says so, because it can use all the memory of the server.
 */
export function PluginResourceLimits({
  plugin,
}: {
  plugin: ServicePluginResponse
}) {
  const limits = plugin.resource_limits
  if (!limits) return null
  const summary = pluginLimitsSummary(limits)
  const path = servicePluginLimitsPath(plugin)
  return (
    <p className="mt-1 text-xs text-muted-foreground [overflow-wrap:anywhere]">
      Limits: {summary.memory} · {summary.cpu}
      {!summary.capped && (
        <span className="text-amber-700 dark:text-amber-500">
          {' '}
          · it can use all the memory of this server
        </span>
      )}
      {path && (
        <>
          {' · '}
          <Link to={path} className="font-medium text-foreground underline">
            {summary.capped ? 'Change limits' : 'Set a limit'}
          </Link>
        </>
      )}
    </p>
  )
}

/**
 * That a plugin is switched off, since when and by whom, and what that
 * leaves as it was.
 */
export function PluginDisabledNote({
  plugin,
  currentUserId,
}: {
  plugin: ServicePluginResponse
  currentUserId?: number
}) {
  if (plugin.state !== 'disabled') return null
  const when = plugin.disabled_at
    ? new Date(plugin.disabled_at).toLocaleString()
    : null
  const who =
    plugin.disabled_by == null
      ? null
      : plugin.disabled_by === currentUserId
        ? 'by you'
        : 'by another administrator'
  return (
    <p className="mt-1 text-xs text-muted-foreground">
      Disabled{who ? ` ${who}` : ''}
      {when ? ` on ${when}` : ''}. Temps sends it nothing and takes no calls
      from it. Its service keeps running and its permissions are kept.
    </p>
  )
}

/** Switch a disabled plugin on again, from its row. */
export function PluginEnableButton({
  plugin,
  pending,
  disabled,
  onEnable,
}: {
  plugin: ServicePluginResponse | undefined
  pending?: boolean
  disabled?: boolean
  onEnable: (name: string) => void
}) {
  if (plugin?.state !== 'disabled') return null
  return (
    <Button
      size="sm"
      disabled={disabled || pending}
      onClick={() => onEnable(plugin.name)}
    >
      {pending ? (
        <Loader2 className="mr-2 size-4 animate-spin" />
      ) : (
        <Power className="mr-2 size-4" />
      )}
      {pending ? 'Enabling…' : 'Enable'}
    </Button>
  )
}
