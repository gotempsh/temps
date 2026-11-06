// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link } from 'react-router'
import { AlertTriangle } from 'lucide-react'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Checkbox } from '@/components/ui/checkbox'
import { CopyButton } from '@/components/ui/copy-button'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Skeleton } from '@/components/ui/skeleton'
import type { WorkerInstall, WorkerOs } from '@/lib/worker-join-script'

const OS_CHOICES: { value: WorkerOs; label: string; description: string }[] = [
  {
    value: 'linux',
    label: 'Linux',
    description:
      'A server, VM or home machine. It dials this server over a WireGuard tunnel it sets up with the Linux kernel, and the agent runs as a systemd service. Installing and joining need sudo.',
  },
  {
    value: 'macos',
    label: 'Mac',
    description:
      'A Mac with Docker Desktop, OrbStack, colima or Rancher Desktop running. Its containers and its end of the tunnel run in that app\u2019s VM, and nothing on the Mac is opened to the network. Runs as you, without sudo, and starts at login. Suited to development and personal use: it goes offline while the Mac sleeps and reconnects when it wakes.',
  },
]

/**
 * Which machine is being added, asked before anyone creates a command: a
 * Linux worker installs and runs as root, a Mac (ADR 090) as the user whose
 * Docker app holds its containers. Both dial this server, so neither needs
 * an address this server can reach.
 */
export function WorkerOsChoice({
  value,
  onChange,
}: {
  value: WorkerOs
  onChange: (value: WorkerOs) => void
}) {
  return (
    <fieldset className="space-y-2">
      <legend className="text-sm font-medium">
        What does the new machine run?
      </legend>
      <RadioGroup
        value={value}
        onValueChange={(next) => onChange(next as WorkerOs)}
        className="gap-2"
      >
        {OS_CHOICES.map((choice) => (
          <div key={choice.value} className="flex items-start gap-2">
            <RadioGroupItem
              id={`join-os-${choice.value}`}
              value={choice.value}
              className="mt-0.5"
            />
            <Label
              htmlFor={`join-os-${choice.value}`}
              className="grid gap-0.5 font-normal"
            >
              <span className="text-sm">{choice.label}</span>
              <span className="text-xs text-muted-foreground">
                {choice.description}
              </span>
            </Label>
          </div>
        ))}
      </RadioGroup>
      <p className="text-xs text-muted-foreground">
        Windows cannot join this way.{' '}
        <code>temps join --private-address &lt;ip&gt;</code> is for a machine
        this server can reach at that address (see{' '}
        <strong>Other ways to add a node</strong> below).
      </p>
    </fieldset>
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
  install,
  pending = false,
  os = 'linux',
}: {
  command: string
  install: WorkerInstall
  /** Still reading the server's release, so the pin is not known yet. */
  pending?: boolean
  os?: WorkerOs
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
      {os === 'macos' ? (
        <p className="text-sm text-muted-foreground">
          Paste this in Terminal on the Mac, as yourself (no <code>sudo</code>
          ), with your Docker app running. Each step runs only if the one before
          it worked. Keep the Docker app starting at login so the worker comes
          back after a restart.
        </p>
      ) : (
        <p className="text-sm text-muted-foreground">
          Paste this on the new machine as a user who can use <code>sudo</code>:
          installing, joining and starting the agent need root. Each step runs
          only if the one before it worked. In a root shell without sudo, leave
          out <code>sudo -H</code>.
        </p>
      )}
      <div className="flex items-start gap-2 rounded-md bg-muted px-3 py-2 font-mono text-xs">
        <pre
          tabIndex={0}
          className="min-w-0 flex-1 overflow-x-auto whitespace-pre"
        >
          {command}
        </pre>
        <CopyButton minimal className="h-6 w-6 shrink-0" value={command} />
      </div>
      <WorkerInstallNote install={install} />
    </div>
  )
}

/** Which release an install command takes, and why. */
export function WorkerInstallNote({ install }: { install: WorkerInstall }) {
  switch (install.kind) {
    case 'release':
      return (
        <p className="text-xs text-muted-foreground">
          Installs <code>{install.tag}</code>, the version this server runs, so
          the worker and the control plane match.
        </p>
      )
    case 'nightly':
      return (
        <p className="text-xs text-muted-foreground">
          This server runs a build made after <code>{install.after}</code>,
          which no release contains, so this installs the newest nightly build
          rather than the older <code>{install.after}</code>. Nightly builds are
          cut every day from the main branch: one has everything merged before
          it was cut.
        </p>
      )
    default:
      return <UnpinnedInstallNote />
  }
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
