// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button, type ButtonProps } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'
import { useState } from 'react'

const INSTALLATIONS = [
  { id: 'apt', label: 'Ubuntu / Debian' },
  { id: 'rpm', label: 'Fedora / RHEL' },
  { id: 'desktop', label: 'Docker Desktop' },
  { id: 'container', label: 'Temps in Docker' },
] as const

type Installation = (typeof INSTALLATIONS)[number]['id']

// Package commands assume Docker's repository is configured. Keep the
// upgrade scoped to Compose; a system-wide package upgrade is unnecessary.
const COMMANDS = {
  apt: 'sudo apt-get update && sudo apt-get install docker-compose-plugin',
  rpm: 'sudo dnf install --refresh docker-compose-plugin',
}

interface ComposeUpgradeHelpProps {
  version?: string | null
  minimumVersion?: string
  variant?: ButtonProps['variant']
  className?: string
}

/** Shared recovery path from the platform banner and deployment diagnostics. */
export function ComposeUpgradeHelp({
  version,
  minimumVersion = '2.35.0',
  variant = 'outline',
  className,
}: ComposeUpgradeHelpProps) {
  const [open, setOpen] = useState(false)
  const [installation, setInstallation] = useState<Installation>('apt')
  const command =
    installation === 'apt' || installation === 'rpm'
      ? COMMANDS[installation]
      : null

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button size="sm" variant={variant} className={className}>
          Update Docker Compose
        </Button>
      </DialogTrigger>
      <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Update Docker Compose</DialogTitle>
          <DialogDescription>
            Temps needs Docker Compose v{minimumVersion} or newer. Ask your
            server administrator to follow these steps if you do not have shell
            access.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4 text-sm">
          {version && (
            <p>
              Detected version: <span className="font-mono">{version}</span>
            </p>
          )}
          <div className="space-y-2">
            <p className="font-medium">1. Choose where Temps runs</p>
            <div
              role="group"
              aria-label="Temps installation"
              className="flex flex-wrap gap-2"
            >
              {INSTALLATIONS.map(({ id, label }) => (
                <Button
                  key={id}
                  size="sm"
                  variant={installation === id ? 'secondary' : 'outline'}
                  aria-pressed={installation === id}
                  onClick={() => setInstallation(id)}
                >
                  {label}
                </Button>
              ))}
            </div>
          </div>

          {command && (
            <div className="space-y-2">
              <p>
                Run this on the Temps server. It installs or updates the Compose
                plugin using Docker&apos;s package repository.
              </p>
              <Command value={command} label="Copy Compose update command" />
              <p className="text-muted-foreground">
                If the package is missing or still too old, follow{' '}
                <a
                  href="https://docs.docker.com/compose/install/linux/#install-using-the-repository"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="underline underline-offset-4"
                >
                  Docker&apos;s repository setup instructions
                </a>
                . For a manually installed plugin, use the{' '}
                <a
                  href="https://docs.docker.com/compose/install/linux/#install-the-plugin-manually"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="underline underline-offset-4"
                >
                  manual installation instructions
                </a>
                . A per-user plugin can take precedence over the system package.
              </p>
            </div>
          )}

          {installation === 'desktop' && (
            <p>
              Update Docker Desktop on the machine running Temps; it includes
              Compose. Open Docker Desktop&apos;s menu and select Check for
              updates, or install the latest release from{' '}
              <a
                href="https://docs.docker.com/desktop/setup/install/"
                target="_blank"
                rel="noopener noreferrer"
                className="underline underline-offset-4"
              >
                Docker&apos;s installation guide
              </a>
              . Applying the update may restart Docker and its containers.
            </p>
          )}

          {installation === 'container' && (
            <p>
              Update the Compose plugin in the image that runs Temps, then
              recreate the Temps container with that image, keeping its existing
              configuration and persistent volumes. Updating Compose on the
              Docker host alone does not update the copy inside the container.
              Use the package manager for the runtime image or{' '}
              <a
                href="https://docs.docker.com/compose/install/linux/#install-the-plugin-manually"
                target="_blank"
                rel="noopener noreferrer"
                className="underline underline-offset-4"
              >
                Docker&apos;s manual plugin installation instructions
              </a>
              .
            </p>
          )}

          <div className="space-y-2">
            <p className="font-medium">2. Verify the version Temps will use</p>
            <p className="text-muted-foreground">
              Run this as the same user and in the same environment as Temps
              (inside the Temps container for a Docker installation). Confirm v
              {minimumVersion} or newer.
            </p>
            <Command
              value="docker compose version"
              label="Copy Compose version command"
            />
          </div>
          <div className="space-y-2">
            <p className="font-medium">3. Restart Temps and retry</p>
            <p className="text-muted-foreground">
              Restart the Temps service or container to run its startup check
              again. Refresh this console, then retry the Compose check or
              failed deployment. The warning clears when Temps detects a
              supported plugin.
            </p>
          </div>
        </div>

        <DialogFooter>
          <DialogClose asChild>
            <Button variant="outline">Close</Button>
          </DialogClose>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function Command({ value, label }: { value: string; label: string }) {
  return (
    <div className="flex items-start gap-2 rounded-md border bg-muted/40 px-3 py-2">
      <code className="min-w-0 flex-1 whitespace-pre-wrap break-all font-mono text-xs">
        {value}
      </code>
      <CopyButton value={value} label={label} minimal className="shrink-0" />
    </div>
  )
}
