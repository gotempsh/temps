// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  emailOverviewSections,
  type EmailOverviewSections,
} from '@/lib/email-setup'
import { useQuery } from '@tanstack/react-query'
import {
  fetchEmailDomains,
  fetchEmailProviders,
  fetchEmailStats,
} from './email-queries'

/** What the setup checklist and the overview's sections are derived from. */
export function useEmailSetupInput() {
  const providers = useQuery({
    queryKey: ['email-providers'],
    queryFn: fetchEmailProviders,
  })
  const domains = useQuery({
    queryKey: ['email-domains'],
    queryFn: fetchEmailDomains,
  })
  const stats = useQuery({
    queryKey: ['email-stats', undefined],
    queryFn: () => fetchEmailStats(),
  })
  return { providers, domains, stats }
}

/**
 * Which overview sections to show. `undefined` while the answers load, so the
 * page holds a placeholder instead of showing sections it then removes. A
 * failed lookup shows everything: each section reports its own error.
 */
export function useEmailOverviewSections(): EmailOverviewSections | undefined {
  const { providers, domains, stats } = useEmailSetupInput()
  // A section that mounts refetches a failed query, which puts it back in
  // loading. Counting it as failed until it has data keeps the sections up
  // instead of swapping them for the placeholder on every retry.
  const failed = (query: {
    isError: boolean
    data?: unknown
    errorUpdateCount: number
  }) =>
    query.isError || (query.data === undefined && query.errorUpdateCount > 0)
  if (failed(providers) || failed(domains) || failed(stats)) {
    return { domains: true, providers: true, sent: true, events: true }
  }
  if (!providers.data || !domains.data || !stats.data) return undefined
  return emailOverviewSections({
    providers: providers.data,
    domains: domains.data,
    stats: stats.data,
  })
}
