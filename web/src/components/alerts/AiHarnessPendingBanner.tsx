import { useQuery } from '@tanstack/react-query'
import { ArrowRight, Terminal } from 'lucide-react'
import { Link, useLocation } from 'react-router'
import { listApiKeysOptions } from '@/api/client/@tanstack/react-query.gen'
import { getAiHarnessStatus } from '@/lib/ai-onboarding'

/**
 * Keeps a half-finished external harness connection visible until the
 * dedicated key makes its first authenticated request. There is deliberately
 * no dismiss action: this is a short-lived setup state, not an announcement.
 */
export function AiHarnessPendingBanner() {
  const { pathname } = useLocation()
  const { data } = useQuery({
    ...listApiKeysOptions({ query: { page: 1, page_size: 100 } }),
    retry: false,
    refetchInterval: (query) =>
      getAiHarnessStatus(query.state.data?.api_keys) === 'waiting'
        ? 10_000
        : false,
  })

  if (
    pathname === '/setup/ai' ||
    pathname.startsWith('/settings/keys') ||
    getAiHarnessStatus(data?.api_keys) !== 'waiting'
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
