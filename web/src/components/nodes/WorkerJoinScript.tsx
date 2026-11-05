// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link } from 'react-router'
import { AlertTriangle, Info } from 'lucide-react'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Checkbox } from '@/components/ui/checkbox'
import { CopyButton } from '@/components/ui/copy-button'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'

/**
 * What the new machine must be, said before anyone creates a command: the
 * outbound join has the worker dial this server over a WireGuard tunnel it
 * brings up itself, which needs the Linux kernel's WireGuard. The other
 * operating systems only join with `--private-address`, which needs this
 * server to reach them.
 */
export function WorkerRequirements() {
  return (
    <Alert>
      <Info className="h-4 w-4" />
      <AlertTitle>The new machine must run Linux</AlertTitle>
      <AlertDescription className="space-y-1 text-xs">
        <p>
          It dials this server over a WireGuard tunnel it sets up itself, which
          needs the Linux kernel&apos;s WireGuard, and the agent runs as a
          systemd service. A Mac or Windows machine cannot join this way.
        </p>
        <p>
          <code>temps join --private-address &lt;ip&gt;</code> is for a machine
          this server can reach at that address, such as one on the same private
          network or with a public IP (see{' '}
          <strong>Other ways to add a node</strong> below). A server on the
          internet cannot reach a machine behind a home router, so use a Linux
          machine for this one. A non-Linux machine joined that way also runs
          without the network policy boot guard (it needs <code>nft</code>) and
          without the systemd service. The commands on this page are for Linux.
        </p>
      </AlertDescription>
    </Alert>
  )
}

/**
 * `temps join` refuses a machine that is already a worker of another
 * control plane unless it gets `--force`. The choice is explicit because
 * moving the machine takes it away from its old cluster.
 */
export function ForceJoinOption({
  checked,
  onChange,
}: {
  checked: boolean
  onChange: (checked: boolean) => void
}) {
  return (
    <div className="space-y-2">
      <div className="flex items-start gap-2">
        <Checkbox
          id="join-force"
          checked={checked}
          onCheckedChange={(next) => onChange(next === true)}
          className="mt-0.5"
        />
        <Label htmlFor="join-force" className="grid gap-0.5 font-normal">
          <span className="text-sm">
            It is already a worker of another Temps cluster: move it here
          </span>
          <span className="text-xs text-muted-foreground">
            Without this, <code>temps join</code> stops on such a machine (or
            one whose old cluster it cannot verify) and changes nothing. With
            it, the command adds <code>--force</code>.
          </span>
        </Label>
      </div>
      {checked && (
        <Alert className="border-amber-500/30 bg-amber-500/5">
          <AlertTriangle className="h-4 w-4 text-amber-500" />
          <AlertTitle className="text-amber-700 dark:text-amber-400">
            The old cluster loses this worker
          </AlertTitle>
          <AlertDescription className="text-amber-700 dark:text-amber-300">
            Joining replaces the machine&apos;s agent config, so it stops
            answering its current control plane. Move or remove what that
            cluster runs on it first, then remove the node there.
          </AlertDescription>
        </Alert>
      )}
    </div>
  )
}

/** The command to paste on the new machine, and which release it installs. */
export function WorkerJoinScript({
  command,
  pinnedTo,
  pending = false,
}: {
  command: string
  pinnedTo: string | null
  /** Still reading the server's release, so the pin is not known yet. */
  pending?: boolean
}) {
  if (pending) {
    return (
      <div className="space-y-2" aria-busy="true">
        <p className="text-sm text-muted-foreground">
          Reading the version this server runs…
        </p>
        <Skeleton className="h-20 w-full" />
      </div>
    )
  }
  return (
    <div className="space-y-2">
      <p className="text-sm text-muted-foreground">
        Paste this on the new machine as a user who can use <code>sudo</code>:
        installing, joining and starting the agent need root. Each step runs
        only if the one before it worked. In a root shell without sudo, leave
        out <code>sudo -H</code>.
      </p>
      <div className="flex items-start gap-2 rounded-md bg-muted px-3 py-2 font-mono text-xs">
        <pre
          tabIndex={0}
          className="min-w-0 flex-1 overflow-x-auto whitespace-pre"
        >
          {command}
        </pre>
        <CopyButton minimal className="h-6 w-6 shrink-0" value={command} />
      </div>
      {pinnedTo ? (
        <p className="text-xs text-muted-foreground">
          Installs <code>{pinnedTo}</code>, the version this server runs, so the
          worker and the control plane match.
        </p>
      ) : (
        <UnpinnedInstallNote />
      )}
    </div>
  )
}

/** Said wherever an install could not be pinned to this server's release. */
export function UnpinnedInstallNote() {
  return (
    <p className="text-xs text-amber-700 dark:text-amber-300">
      This server&apos;s release could not be read, so the install takes the
      newest stable release, which may not match it. To pin it, add the version
      shown on the{' '}
      <Link to="/settings/version" className="font-medium underline">
        Version page
      </Link>
      : <code>bash -s -- &lt;version&gt;</code>.
    </p>
  )
}
