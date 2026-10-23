// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router'
import {
  FunctionRuntimes,
  FunctionsOnboarding,
} from '@/components/functions/FunctionsOnboarding'
import { PageHeader } from '@/components/layout/PageContainer'
import { ReadFailure } from '@/components/ui/read-failure'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { functionsKeys, getFunctionsCapability } from '@/lib/functions'
import { isForbiddenRead } from '@/lib/read-failure'

/**
 * Which function runtimes this instance has, and how to enable a missing
 * one. The functions capability's `setup_path` points here.
 */
export function FunctionsSettingsPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  usePageTitle('Functions')
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Functions' },
    ])
  }, [setBreadcrumbs])

  const query = useQuery({
    queryKey: functionsKeys.capability,
    queryFn: getFunctionsCapability,
    meta: { errorTitle: 'Could not check the function runtimes' },
  })

  return (
    <div className="w-full min-w-0 space-y-6">
      <PageHeader
        title="Functions"
        description="The runtimes that run functions on this instance. Functions themselves are created on each project's Functions page."
      />
      {query.isPending ? (
        <div className="space-y-3" aria-busy="true">
          <Skeleton className="h-24 w-full" />
          <Skeleton className="h-16 w-full" />
        </div>
      ) : query.isError ? (
        <ReadFailure
          resource="Function runtimes"
          error={query.error}
          onRetry={
            isForbiddenRead(query.error)
              ? undefined
              : () => void query.refetch()
          }
          retrying={query.isFetching}
        />
      ) : query.data.configured ? (
        <div className="space-y-4">
          <p className="text-sm text-muted-foreground" role="status">
            At least one runtime is available: functions can be created from a
            project&apos;s Functions page (open a project, then{' '}
            <span className="font-medium">Functions</span> in its menu).
          </p>
          <FunctionRuntimes capability={query.data} />
        </div>
      ) : (
        <FunctionsOnboarding capability={query.data} showSetupLink={false} />
      )}
      <p className="max-w-3xl text-sm text-muted-foreground">
        A function&apos;s network access follows its network policy: by default
        it may reach anything. Restrict it under{' '}
        <Link
          to="/settings/network-policy"
          className="underline underline-offset-2"
        >
          Network policy
        </Link>
        .
      </p>
    </div>
  )
}
