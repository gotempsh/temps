// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { ChevronLeft, RefreshCw } from 'lucide-react'
import { getProjectGroupBySlugOptions } from '@/api/client/@tanstack/react-query.gen'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { ConnectRepositoryPanel } from '@/components/project-groups/ConnectRepositoryPanel'
import {
  groupHref,
  IMPORT_AFTER_CONNECT,
  IMPORT_PARAM,
  THEN_PARAM,
} from '@/lib/terminology'
import { extractProblemDetails, getErrorMessage } from '@/utils/errorHandling'

/**
 * Full page for pointing a project at its repository: the same repository
 * picker as the new-service flow, plus the branch to follow.
 * `?then=import` continues into "add services from the repository" once the
 * repository is saved.
 */
export function ConnectGroupRepository() {
  const { slug = '' } = useParams()
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const importAfter = searchParams.get(THEN_PARAM) === IMPORT_AFTER_CONNECT

  const {
    data: group,
    isLoading,
    isError,
    error,
    refetch,
    isFetching,
  } = useQuery({
    ...getProjectGroupBySlugOptions({ path: { slug } }),
    retry: false,
  })

  const title = group?.repository ? 'Change repository' : 'Connect a repository'

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: group?.name ?? slug, href: groupHref(slug) },
      { label: title },
    ])
  }, [group?.name, setBreadcrumbs, slug, title])

  usePageTitle(title)

  const backHref = groupHref(slug)
  const back = (
    <Button variant="ghost" size="sm" asChild className="-ml-2 w-fit">
      <Link to={backHref}>
        <ChevronLeft className="size-4" />
        Back to {group?.name ?? 'project'}
      </Link>
    </Button>
  )

  if (isLoading) {
    return (
      <PageContainer aria-busy="true">
        <Skeleton className="h-8 w-40" />
        <Skeleton className="h-9 w-72" />
        <Skeleton className="h-5 w-full max-w-xl" />
        <Skeleton className="h-96 w-full" />
      </PageContainer>
    )
  }

  if (isError || !group) {
    const notFound = /not found/i.test(
      extractProblemDetails(error)?.detail ?? ''
    )
    return (
      <PageContainer>
        <Alert variant={notFound ? 'default' : 'destructive'}>
          <AlertTitle>
            {notFound
              ? 'Project not found'
              : 'This project could not be loaded'}
          </AlertTitle>
          <AlertDescription>
            <p>
              {notFound
                ? `There is no project “${slug}”, or none of its services are visible to you.`
                : getErrorMessage(
                    error,
                    'Check your connection and try again.'
                  )}
            </p>
            <div className="mt-3 flex gap-2">
              <Button variant="outline" size="sm" asChild>
                <Link to="/projects">Back to projects</Link>
              </Button>
              {!notFound && (
                <Button
                  variant="outline"
                  size="sm"
                  disabled={isFetching}
                  onClick={() => void refetch()}
                >
                  <RefreshCw
                    className={isFetching ? 'size-4 animate-spin' : 'size-4'}
                  />
                  Retry
                </Button>
              )}
            </div>
          </AlertDescription>
        </Alert>
      </PageContainer>
    )
  }

  return (
    <PageContainer>
      {back}
      <PageHeader
        title={title}
        description={
          <>
            Connect the repository once, then add each app in it (say{' '}
            <code>apps/web</code> and <code>apps/api</code>) as a service.
            Services follow {group.name}&apos;s branch and each keeps its own
            directory.
          </>
        }
      />
      <ConnectRepositoryPanel
        key={group.id}
        group={group}
        cancelHref={backHref}
        onConnected={() =>
          navigate(importAfter ? `${backHref}?${IMPORT_PARAM}=1` : backHref)
        }
      />
    </PageContainer>
  )
}
