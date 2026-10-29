// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// A deploy finished and nobody can reach the environment (ADR 068 D5): say
// so once, and offer one button per fix. The deploy itself succeeded; this
// never reads as a failure. The tunnel fix links to the guided, undoable
// "Make it reachable" page with that host (ADR 068 D3, ADR 064, ADR 069).

import { Globe } from 'lucide-react'
import { Link } from 'react-router'
import type { ReachabilityFix, ReachabilityWarning } from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { reachabilityPath } from '@/lib/reachability-page'

export function ReachabilityWarningBanner({
  warning,
  projectSlug,
  className,
}: {
  warning: ReachabilityWarning | null | undefined
  /** Lets the tunnel fix open Make it reachable instead of the tunnels. */
  projectSlug?: string
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
              projectSlug={projectSlug}
              environmentId={warning.environment_id}
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
  projectSlug,
  environmentId,
}: {
  fix: ReachabilityFix
  primary: boolean
  projectSlug?: string
  environmentId: number
}) {
  const href =
    fix.kind === 'tunnel_environment_hosts' &&
    fix.hostname &&
    projectSlug !== undefined
      ? reachabilityPath(projectSlug, environmentId, {
          audience: 'public',
          address: fix.hostname,
          way: 'tunnel_environment_hosts',
        })
      : fix.href
  // The tunnel fix opens the page even when it also names a command.
  if (fix.command && href === fix.href) {
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
  if (!href) return null
  return (
    <Button
      size="sm"
      variant={primary ? 'default' : 'outline'}
      className="w-full sm:w-auto"
      asChild
    >
      <Link to={href}>{fix.label}</Link>
    </Button>
  )
}
