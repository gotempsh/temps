// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  AccessDifference,
  PluginServicesResponse,
  ServicePluginResponse,
} from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { useApprovePluginUpdate } from '@/hooks/usePlugins'
import { pluginPermissionLabels } from '@/lib/plugin-grants'
import {
  addServiceFailure,
  pluginUpdateRequests,
  pluginUpdateVersions,
  servicePluginPath,
  servicePluginsWithUpdate,
} from '@/lib/plugin-services'
import {
  GitCompareArrows,
  Loader2,
  ShieldAlert,
  ShieldCheck,
} from 'lucide-react'
import { Link } from 'react-router'
import { toast } from 'sonner'

/**
 * Which build of its service a plugin is pinned to: the one an administrator
 * approved. Temps attaches to no other.
 */
export function PluginPinnedBuild({
  plugin,
}: {
  plugin: ServicePluginResponse
}) {
  if (plugin.pinned_deployment_id == null) return null
  return (
    <p className="mt-1 text-xs text-muted-foreground [overflow-wrap:anywhere]">
      Pinned to deployment #{plugin.pinned_deployment_id}
      {plugin.image ? ` of the image ${plugin.image}` : ''}
      {plugin.pending_update
        ? ` · deployment #${plugin.pending_update.deployment_id} waits for approval`
        : ' · a new deployment will wait for approval'}
    </p>
  )
}

/**
 * The plugins whose service runs a build nobody approved. Each is
 * disconnected until its update is reviewed, so it is said before the list.
 */
export function PluginUpdateNotice({
  services,
  disabled,
  onReview,
}: {
  services: PluginServicesResponse | undefined
  disabled?: boolean
  onReview: (name: string) => void
}) {
  const waiting = servicePluginsWithUpdate(services?.plugins ?? [])
  if (waiting.length === 0) return null
  return (
    <Alert>
      <GitCompareArrows className="size-4" />
      <AlertTitle>
        {waiting.length === 1
          ? '1 plugin update waits for approval'
          : `${waiting.length} plugin updates wait for approval`}
      </AlertTitle>
      <AlertDescription>
        <ul className="space-y-2">
          {waiting.map((plugin) => (
            <li
              key={plugin.name}
              className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between"
            >
              <span className="min-w-0 [overflow-wrap:anywhere]">
                <strong>{plugin.name}:</strong> its service was redeployed. The
                plugin is disconnected until you approve the new build
                {plugin.pending_update?.difference.asks_for_more
                  ? ', which asks for more access.'
                  : '.'}
              </span>
              <Button
                variant="outline"
                size="sm"
                className="shrink-0"
                disabled={disabled}
                onClick={() => onReview(plugin.name)}
              >
                Review update
              </Button>
            </li>
          ))}
        </ul>
      </AlertDescription>
    </Alert>
  )
}

/** What an update asks for compared with the approved build. */
export function PluginUpdateDifference({
  difference,
}: {
  difference: AccessDifference
}) {
  const requests = pluginUpdateRequests(difference)
  return (
    <div className="space-y-3 text-sm">
      {requests.length === 0 ? (
        <p className="flex items-start gap-2 rounded-lg border p-3">
          <ShieldCheck
            className="mt-0.5 size-4 shrink-0 text-muted-foreground"
            aria-hidden="true"
          />
          <span>
            Asks for no new access. It is still new code that would run with the
            permissions this plugin already has.
          </span>
        </p>
      ) : (
        <div className="space-y-3 rounded-lg border border-amber-500/50 p-3">
          <p className="flex items-start gap-2 font-medium">
            <ShieldAlert
              className="mt-0.5 size-4 shrink-0 text-amber-600"
              aria-hidden="true"
            />
            Asks for more than the approved build
          </p>
          {requests.map((request) => (
            <div key={request.title}>
              <p className="text-muted-foreground">{request.title}</p>
              <ul className="mt-1 list-disc space-y-0.5 pl-5">
                {request.items.map((item) => (
                  <li key={item} className="[overflow-wrap:anywhere]">
                    {item}
                  </li>
                ))}
              </ul>
            </div>
          ))}
          {difference.added_permissions.length > 0 && (
            <p className="text-muted-foreground">
              Approving does not allow new permissions. Choose them in
              Permissions afterwards.
            </p>
          )}
        </div>
      )}
      {difference.removed_permissions.length > 0 && (
        <p className="text-muted-foreground">
          No longer asks for:{' '}
          {difference.removed_permissions
            .map((permission) => pluginPermissionLabels[permission].label)
            .join(', ')}
          .
        </p>
      )}
    </div>
  )
}

/**
 * Review the update of a plugin and approve it. Nothing is approved without
 * this dialog: closing it leaves the plugin on its approved build,
 * disconnected until that build runs again.
 */
export function PluginUpdateDialog({
  plugin,
  open,
  onOpenChange,
  onSensitiveError,
  onApproved,
}: {
  plugin: ServicePluginResponse | undefined
  open: boolean
  onOpenChange: (open: boolean) => void
  onSensitiveError: (error: unknown, retry: () => void) => boolean
  /** The plugin whose update was approved, and whether it asks for more. */
  onApproved?: (name: string, asksForMore: boolean) => void
}) {
  const approve = useApprovePluginUpdate()
  const update = plugin?.pending_update
  const servicePath = plugin ? servicePluginPath(plugin) : undefined
  async function submit() {
    if (!plugin || !update) return
    try {
      const approved = await approve.mutateAsync({
        name: plugin.name,
        review: update.review,
      })
      toast.success(
        `${approved.name} now runs deployment #${approved.deployment_id}.`
      )
      onOpenChange(false)
      onApproved?.(approved.name, approved.difference.asks_for_more)
    } catch (error) {
      // Any other failure is shown by the app-wide mutation error handler
      // with the server's reason, and repeated in the dialog.
      onSensitiveError(error, () => void submit())
    }
  }
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!approve.isPending) {
          approve.reset()
          onOpenChange(next)
        }
      }}
    >
      <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Update for {plugin?.name}</DialogTitle>
          <DialogDescription>
            A plugin runs only the build you approved. Its service now runs
            another one, so the plugin is disconnected until you decide.
          </DialogDescription>
        </DialogHeader>
        {plugin && update ? (
          <div className="space-y-4">
            <dl className="grid gap-x-4 gap-y-1 text-sm sm:grid-cols-[auto_1fr]">
              <dt className="text-muted-foreground">Version</dt>
              <dd>{pluginUpdateVersions(update.difference)}</dd>
              <dt className="text-muted-foreground">Approved build</dt>
              <dd className="[overflow-wrap:anywhere]">
                {plugin.pinned_deployment_id != null
                  ? `Deployment #${plugin.pinned_deployment_id}`
                  : 'Not recorded'}
                {plugin.image ? ` · ${plugin.image}` : ''}
              </dd>
              <dt className="text-muted-foreground">New build</dt>
              <dd className="[overflow-wrap:anywhere]">
                Deployment #{update.deployment_id}
                {update.image ? ` · ${update.image}` : ''}
              </dd>
            </dl>
            <PluginUpdateDifference difference={update.difference} />
            <p className="text-sm text-muted-foreground">
              To keep the approved build instead, roll the service back to it.
              {servicePath && (
                <>
                  {' '}
                  <Link className="underline" to={servicePath}>
                    Open service
                  </Link>
                </>
              )}
            </p>
            {approve.isError && (
              <p role="alert" className="text-sm break-words text-destructive">
                {addServiceFailure(approve.error)}
              </p>
            )}
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">
            No update waits for this plugin. It runs the build you approved.
          </p>
        )}
        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            disabled={approve.isPending}
            onClick={() => onOpenChange(false)}
          >
            Not now
          </Button>
          <Button
            type="button"
            disabled={!update || approve.isPending}
            onClick={() => void submit()}
          >
            {approve.isPending && (
              <Loader2 className="mr-2 size-4 animate-spin" />
            )}
            {approve.isPending ? 'Connecting…' : 'Approve update'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
