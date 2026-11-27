// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  PluginIsolation,
  PluginIsolationResponse,
  PluginSandboxResponse,
} from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
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
import { Checkbox } from '@/components/ui/checkbox'
import { Label } from '@/components/ui/label'
import { isolationActionLabel, isolationSummary } from '@/lib/plugin-isolation'
import { Status } from '@temps-sdk/ds'
import { AlertCircle, ShieldAlert } from 'lucide-react'

/** The isolation of one running plugin, as a status in its row. */
export function PluginIsolationStatus({
  isolation,
}: {
  isolation: PluginIsolationResponse | undefined
}) {
  if (!isolation?.effective) return null
  const summary = isolationSummary(isolation.effective)
  return (
    <span title={summary.detail}>
      <Status tone={summary.tone} label={summary.label} />
    </span>
  )
}

/**
 * What an administrator needs to know before plugins are listed: that this
 * server cannot contain them (and why), and which installed plugins did not
 * start (and why). Renders nothing when there is nothing to act on.
 */
export function PluginSandboxNotice({
  sandbox,
  unavailable,
}: {
  sandbox: PluginSandboxResponse | undefined
  /** The sandbox status itself could not be read. */
  unavailable: boolean
}) {
  if (unavailable) {
    return (
      <Alert>
        <AlertCircle className="size-4" />
        <AlertTitle>Plugin isolation is unknown</AlertTitle>
        <AlertDescription>
          Temps could not read how plugins are isolated on this server. Reload
          the page to try again.
        </AlertDescription>
      </Alert>
    )
  }
  if (!sandbox) return null
  const stopped = sandbox.plugins.filter(
    (plugin) => !plugin.effective && plugin.start_failure
  )
  if (sandbox.configured && stopped.length === 0) return null
  return (
    <div className="space-y-3">
      {!sandbox.configured && (
        <Alert>
          <ShieldAlert className="size-4" />
          <AlertTitle>Plugins are not contained on this server</AlertTitle>
          <AlertDescription className="break-words">
            {sandbox.reason ?? 'This server cannot run plugins in containers.'}{' '}
            A new plugin installs only if you choose to run it without a
            container.
          </AlertDescription>
        </Alert>
      )}
      {stopped.length > 0 && (
        <Alert variant="destructive">
          <AlertCircle className="size-4" />
          <AlertTitle>
            {stopped.length === 1
              ? '1 installed plugin is not running'
              : `${stopped.length} installed plugins are not running`}
          </AlertTitle>
          <AlertDescription>
            <ul className="list-disc space-y-1 pl-4">
              {stopped.map((plugin) => (
                <li key={plugin.name} className="break-words">
                  <strong>{plugin.name}:</strong> {plugin.start_failure}
                </li>
              ))}
            </ul>
          </AlertDescription>
        </Alert>
      )}
    </div>
  )
}

/**
 * The install-time choice: a new plugin runs in a container unless the
 * administrator opts out here. Always rendered, so the choice and its
 * consequence are visible before installing; when this server cannot contain
 * plugins it says so instead of failing after the build.
 */
export function PluginIsolationChoice({
  id,
  sandbox,
  uncontained,
  onChange,
  disabled,
}: {
  id: string
  sandbox: PluginSandboxResponse | undefined
  uncontained: boolean
  onChange: (uncontained: boolean) => void
  disabled?: boolean
}) {
  const containerAvailable = sandbox ? sandbox.configured : true
  return (
    <div className="space-y-2">
      <div className="flex items-start gap-3">
        <Checkbox
          id={id}
          className="mt-0.5"
          checked={uncontained}
          onCheckedChange={(checked) => onChange(checked === true)}
          disabled={disabled}
        />
        <Label htmlFor={id} className="font-normal leading-relaxed">
          Run without a container. The plugin then runs as the Temps account and
          can reach everything Temps can. Needed only by a plugin that asks for
          the Temps database or data directory.
        </Label>
      </div>
      {!containerAvailable && (
        <p role="note" className="break-words text-sm text-muted-foreground">
          This server cannot run plugins in containers:{' '}
          {sandbox?.reason ?? 'no container runtime is available.'} Without this
          option the installation is refused.
        </p>
      )}
    </div>
  )
}

/**
 * Confirmation before a plugin is moved into a container or out of one. It
 * names the plugin and says what the move changes; the plugin restarts.
 * Controlled by `target`: keep the pending move in the caller's state.
 */
export function PluginIsolationDialog({
  name,
  target,
  isPending,
  onConfirm,
  onOpenChange,
}: {
  name: string
  target: PluginIsolation | null
  isPending: boolean
  onConfirm: () => void
  onOpenChange: (open: boolean) => void
}) {
  const intoContainer = target === 'container'
  return (
    <AlertDialog
      open={target !== null}
      onOpenChange={(open) => {
        if (isPending && !open) return
        onOpenChange(open)
      }}
    >
      <AlertDialogContent className="grid-cols-[minmax(0,1fr)]">
        <AlertDialogHeader>
          <AlertDialogTitle className="[overflow-wrap:anywhere]">
            {intoContainer
              ? `Move ${name} to a container?`
              : `Run ${name} without a container?`}
          </AlertDialogTitle>
          <AlertDialogDescription asChild>
            <div className="space-y-2">
              <p>
                {intoContainer
                  ? isolationSummary('container').detail
                  : isolationSummary('process').detail}
              </p>
              <p>
                {intoContainer
                  ? 'The plugin restarts. It keeps its own data, but it no longer receives the Temps database or data directory; a plugin that needs them will not start and is returned to how it runs now.'
                  : 'The plugin restarts. Only do this for a plugin you trust as much as Temps itself.'}
              </p>
            </div>
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel disabled={isPending}>Cancel</AlertDialogCancel>
          <AlertDialogAction
            disabled={isPending}
            onClick={(event) => {
              // Stay open until the restart settles; the caller closes it.
              event.preventDefault()
              onConfirm()
            }}
          >
            {isPending
              ? 'Restarting…'
              : isolationActionLabel(target ?? 'container')}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
