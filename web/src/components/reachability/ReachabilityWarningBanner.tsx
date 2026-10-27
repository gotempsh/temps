// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// A deploy finished and nobody can reach the environment (ADR 068 D5): say
// so once, and offer one button per fix. The deploy itself succeeded; this
// never reads as a failure. The tunnel fix opens the guided, undoable
// "Make it reachable" run with that host (ADR 068 D3, ADR 064).

import { Globe } from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'
import type { ReachabilityFix, ReachabilityWarning } from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { MakeReachableDialog } from './MakeReachableDialog'

export function ReachabilityWarningBanner({
  warning,
  projectId,
  className,
}: {
  warning: ReachabilityWarning | null | undefined
  /** Lets the tunnel fix run in place instead of linking to the tunnels. */
  projectId?: number
  className?: string
}) {
  if (!warning) return null
  return (
    <Alert variant="warning" className={className}>
      <Globe className="h-4 w-4" />
      <AlertTitle>{warning.headline}</AlertTitle>
      <AlertDescription className="flex flex-col gap-3">
        <span>{warning.reason}</span>
        <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap">
          {warning.fixes.map((fix, index) => (
            <FixAction
              key={`${fix.kind}-${index}`}
              fix={fix}
              primary={index === 0}
              projectId={projectId}
              environmentId={warning.environment_id}
              environmentName={warning.environment}
            />
          ))}
        </div>
      </AlertDescription>
    </Alert>
  )
}

function FixAction({
  fix,
  primary,
  projectId,
  environmentId,
  environmentName,
}: {
  fix: ReachabilityFix
  primary: boolean
  projectId?: number
  environmentId: number
  environmentName: string
}) {
  if (
    fix.kind === 'tunnel_environment_hosts' &&
    fix.hostname &&
    projectId !== undefined
  ) {
    return (
      <TunnelFixAction
        fix={fix}
        hostname={fix.hostname}
        primary={primary}
        projectId={projectId}
        environmentId={environmentId}
        environmentName={environmentName}
      />
    )
  }
  if (fix.command) {
    return (
      <div className="flex min-w-0 items-center gap-2 rounded-md border bg-background px-2 py-1">
        <span className="text-sm">{fix.label}:</span>
        <code className="truncate font-mono text-xs">{fix.command}</code>
        <CopyButton
          value={fix.command}
          label={`Copy ${fix.command}`}
          className="shrink-0"
        />
      </div>
    )
  }
  if (!fix.href) return null
  return (
    <Button
      size="sm"
      variant={primary ? 'default' : 'outline'}
      className="w-full sm:w-auto"
      asChild
    >
      <Link to={fix.href}>{fix.label}</Link>
    </Button>
  )
}

function TunnelFixAction({
  fix,
  hostname,
  primary,
  projectId,
  environmentId,
  environmentName,
}: {
  fix: ReachabilityFix
  hostname: string
  primary: boolean
  projectId: number
  environmentId: number
  environmentName: string
}) {
  const [open, setOpen] = useState(false)
  return (
    <>
      <Button
        size="sm"
        variant={primary ? 'default' : 'outline'}
        className="w-full sm:w-auto"
        onClick={() => setOpen(true)}
      >
        {fix.label}
      </Button>
      <MakeReachableDialog
        open={open}
        onOpenChange={setOpen}
        projectId={projectId}
        environmentId={environmentId}
        environmentName={environmentName}
        initialAddress={hostname}
        initialOption="tunnel_environment_hosts"
      />
    </>
  )
}
