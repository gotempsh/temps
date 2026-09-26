// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Info } from 'lucide-react'
import { Card, CardContent } from '@/components/ui/card'

/**
 * A small, always-truthful notice for a page whose feature is disabled by
 * this server's serve profile rather than by missing configuration.
 *
 * Unlike an "unconfigured" onboarding state (which links to a settings page
 * that would fix it), a control-plane profile runs no local workloads by
 * design — there is nothing to configure here, so this never links anywhere.
 * It only renders when `available` is false, so callers can mount it
 * unconditionally at the top of a page and let it decide.
 */
export function PlatformFeatureNotice({
  available,
  label,
}: {
  available: boolean
  label: string
}) {
  if (available) return null

  return (
    <Card className="border-dashed">
      <CardContent className="flex items-start gap-3 px-4 py-3 text-sm text-muted-foreground">
        <Info className="mt-0.5 size-4 shrink-0" />
        <p>
          {label} is not available in this control-plane profile. This server
          runs no local workloads of its own — applications and their
          resources run on worker nodes joined with{' '}
          <code className="rounded bg-muted px-1 py-0.5 text-xs">
            temps join
          </code>
          .
        </p>
      </CardContent>
    </Card>
  )
}
