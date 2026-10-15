// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getComposeStatusOptions } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { composeHostWarning } from '@/lib/compose-check'
import { useQuery } from '@tanstack/react-query'
import { TriangleAlert, X } from 'lucide-react'
import { useState } from 'react'

/**
 * Platform warning when the host's Docker Compose CLI is missing or too old
 * for the `docker compose config` options Compose deployments need. The
 * server probes it once at startup; the banner states the installed version,
 * what is missing and the minimum version to install.
 */
export function ComposeVersionAlert() {
  // The probe runs once at startup, so there is nothing new to fetch until
  // the server restarts.
  const { data } = useQuery({
    ...getComposeStatusOptions(),
    staleTime: Infinity,
    retry: false,
  })
  const [isDismissed, setIsDismissed] = useState(false)
  const warning = composeHostWarning(data)

  if (!warning || isDismissed) {
    return null
  }

  return (
    <div className="flex items-center gap-2 border-b border-orange-200 bg-orange-50/60 px-4 py-1.5 text-sm text-orange-800 dark:border-orange-900/50 dark:bg-orange-950/20 dark:text-orange-200">
      <TriangleAlert className="h-4 w-4 shrink-0 text-orange-600 dark:text-orange-400" />
      <p className="min-w-0 flex-1 truncate" title={warning}>
        <span className="font-medium">Docker Compose needs an upgrade</span>
        <span className="hidden sm:inline"> — {warning}</span>
      </p>
      <Button
        size="sm"
        variant="ghost"
        onClick={() => setIsDismissed(true)}
        aria-label="Dismiss Docker Compose warning"
        className="-mr-1 h-6 w-6 shrink-0 p-0 text-orange-600/70 hover:text-orange-800 dark:text-orange-400/70 dark:hover:text-orange-200"
      >
        <X className="h-3.5 w-3.5" />
      </Button>
    </div>
  )
}
