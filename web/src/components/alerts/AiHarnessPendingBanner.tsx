// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { ArrowRight, Terminal } from 'lucide-react'
import { Link, useLocation } from 'react-router'
import { getAiHarnessStatusOptions } from '@/api/client/@tanstack/react-query.gen'
import { useManualHarnessCompletion } from '@/hooks/useManualHarnessCompletion'
import {
  harnessPollInterval,
  harnessStatusOf,
  shouldShowHarnessPendingBanner,
} from '@/lib/ai-onboarding'
import { ROUTE_FAILURE_IGNORE } from '@/lib/route-query-failures'

/**
 * Keeps a half-finished external harness connection visible until it
 * resolves: the server sees the harness authenticate (with the dedicated key
 * or a browser-approved CLI login made after it), the user removes the
 * pending key on /setup/ai, or marks the harness step done in the checklist.
 * There is deliberately no dismiss action here: this is a short-lived setup
 * state, and /setup/ai is where it is resolved.
 */
export function AiHarnessPendingBanner() {
  const { pathname } = useLocation()
  const { completed: completedManually } = useManualHarnessCompletion()
  const { data } = useQuery({
    ...getAiHarnessStatusOptions(),
    retry: false,
    // An optional setup banner on every page: roles without API-key access
    // are refused here, and that is not a failure of the page they are on.
    // `useActivationSignals` observes the same entry and must opt out too.
    meta: { routeFailure: ROUTE_FAILURE_IGNORE },
    refetchInterval: (query) => harnessPollInterval(query.state.data),
  })

  if (
    !shouldShowHarnessPendingBanner({
      status: harnessStatusOf(data),
      pathname,
      completedManually,
    })
  ) {
    return null
  }

  return (
    <div className="flex items-center gap-2 border-b border-amber-200 bg-amber-50/70 px-4 py-1.5 text-sm text-amber-950 dark:border-amber-900/60 dark:bg-amber-950/20 dark:text-amber-100">
      <Terminal className="size-4 shrink-0 text-amber-700 dark:text-amber-300" />
      <p className="min-w-0 flex-1 truncate">
        <span className="font-medium">Finish connecting your AI harness</span>
        <span className="hidden sm:inline">
          {' '}
          — the key exists but has not authenticated yet.
        </span>
      </p>
      <Link
        to="/setup/ai"
        className="inline-flex shrink-0 items-center gap-1 font-medium underline-offset-2 hover:underline"
      >
        Verify now
        <ArrowRight className="size-3.5" />
      </Link>
    </div>
  )
}
