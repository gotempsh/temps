// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import {
  Link,
  Navigate,
  Route,
  Routes,
  useLocation,
  useNavigate,
  useParams,
  useSearchParams,
} from 'react-router'
import { useQuery } from '@tanstack/react-query'
import {
  ChevronDown,
  FolderGit2,
  Plus,
  RefreshCw,
  Rocket,
  Rows3,
} from 'lucide-react'
import type { ProjectGroupResponse } from '@/api/client/types.gen'
import { getProjectGroupBySlugOptions } from '@/api/client/@tanstack/react-query.gen'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { extractProblemDetails, getErrorMessage } from '@/utils/errorHandling'
import {
  groupConnectRepositoryHref,
  groupHref,
  IMPORT_PARAM,
  serviceCount,
} from '@/lib/terminology'
import { AddServicesDialog } from '@/components/project-groups/AddServicesDialog'
import { GroupServicesPanel } from '@/components/project-groups/GroupServicesPanel'
import { GroupVariablesPanel } from '@/components/project-groups/GroupVariablesPanel'
import { GroupSettingsPanel } from '@/components/project-groups/GroupSettingsPanel'
import { GroupRepositoryPanel } from '@/components/project-groups/GroupRepositoryPanel'
import { ImportServicesDialog } from '@/components/project-groups/ImportServicesDialog'
import { newServiceInGroupHref } from '@/components/project-groups/useJoinGroupAfterCreate'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  GroupBackupsPage,
  GroupDeploymentsPage,
  GroupDomainsPage,
  GroupObservePage,
} from '@/components/project-groups/GroupScopedPages'
import { GroupDatabasesPage } from '@/components/project-groups/GroupDatabasesPage'
import { legacyGroupTabRedirect } from '@/lib/console-scope'

/**
 * A project (group): `/p/:slug` is its overview, and its own pages (the
 * project sidebar's entries) live below it.
 */
export function ProjectGroupDetail() {
  const { slug = '' } = useParams()
  const { search, hash } = useLocation()
  const legacyTab = legacyGroupTabRedirect(slug, search, hash)
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

  // `?tab=variables` / `?tab=settings` were tabs on the overview; each is a
  // page of the project's sidebar now.
  if (legacyTab) return <Navigate to={legacyTab} replace />

  if (isLoading) {
    return (
      <PageContainer innerClassName="space-y-6" aria-busy="true">
        <Skeleton className="h-9 w-64" />
        <Skeleton className="h-5 w-96" />
        <Skeleton className="h-64 w-full" />
      </PageContainer>
    )
  }

  if (isError || !group) {
    // The problem body carries no status code; a hidden or missing group
    // is reported as "not found" by design.
    const notFound = /not found/i.test(
      extractProblemDetails(error)?.detail ?? ''
    )
    return (
      <PageContainer innerClassName="space-y-6">
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
    <Routes>
      <Route index element={<GroupOverview group={group} />} />
      <Route
        path="deployments"
        element={<GroupDeploymentsPage group={group} />}
      />
      <Route
        path="logs"
        element={<GroupObservePage group={group} kind="logs" />}
      />
      <Route
        path="errors"
        element={<GroupObservePage group={group} kind="errors" />}
      />
      <Route
        path="traces"
        element={<GroupObservePage group={group} kind="traces" />}
      />
      <Route
        path="analytics"
        element={<GroupObservePage group={group} kind="analytics" />}
      />
      <Route path="domains" element={<GroupDomainsPage group={group} />} />
      <Route path="databases" element={<GroupDatabasesPage group={group} />} />
      <Route path="backups" element={<GroupBackupsPage group={group} />} />
      <Route path="variables" element={<GroupVariablesPage group={group} />} />
      <Route path="settings" element={<GroupSettingsPage group={group} />} />
      <Route
        path="*"
        element={<Navigate to={groupHref(group.slug)} replace />}
      />
    </Routes>
  )
}

/** Breadcrumbs and title of one of the project's own pages. */
function useGroupChrome(group: ProjectGroupResponse, page?: string) {
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: group.name, href: groupHref(group.slug) },
      ...(page ? [{ label: page }] : []),
    ])
  }, [group.name, group.slug, page, setBreadcrumbs])
  usePageTitle(page ? `${page} · ${group.name}` : group.name)
}

function GroupVariablesPage({ group }: { group: ProjectGroupResponse }) {
  useGroupChrome(group, 'Shared variables')
  return (
    <PageContainer innerClassName="space-y-6">
      <PageHeader title="Shared variables" />
      <GroupVariablesPanel group={group} />
    </PageContainer>
  )
}

function GroupSettingsPage({ group }: { group: ProjectGroupResponse }) {
  useGroupChrome(group, 'Settings')
  const navigate = useNavigate()
  return (
    <PageContainer innerClassName="space-y-6">
      <PageHeader title="Settings" />
      <GroupSettingsPanel
        group={group}
        onConnectRepository={() =>
          navigate(groupConnectRepositoryHref(group.slug))
        }
      />
    </PageContainer>
  )
}

/** The project's overview: its repository, services, routes and databases. */
function GroupOverview({ group }: { group: ProjectGroupResponse }) {
  const slug = group.slug
  useGroupChrome(group)
  const [searchParams, setSearchParams] = useSearchParams()
  const [adding, setAdding] = useState(false)
  const navigate = useNavigate()
  const [importing, setImporting] = useState(false)
  // "From the repository" without a repository connects one on its own page
  // first, which returns here with `?import=1` to continue into the import.
  const importRequested = searchParams.get(IMPORT_PARAM) === '1'
  const connectHref = groupConnectRepositoryHref(slug)

  // Arriving with `?import=1` opens the import; closing it drops the
  // parameter so a reload does not open it again.
  const importOpen = importing || (importRequested && Boolean(group.repository))
  const setImportOpen = (open: boolean) => {
    setImporting(open)
    if (!open && importRequested) {
      setSearchParams(
        (prev) => {
          const next = new URLSearchParams(prev)
          next.delete(IMPORT_PARAM)
          return next
        },
        { replace: true }
      )
    }
  }

  return (
    <PageContainer innerClassName="space-y-6">
      <PageHeader
        title={group.name}
        description={group.description ?? serviceCount(group.projects.length)}
        actions={
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button>
                <Plus className="size-4" />
                Add services
                <ChevronDown className="size-4" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-72">
              <DropdownMenuItem className="items-start" asChild>
                <Link to={newServiceInGroupHref(group.id)}>
                  <Rocket className="mt-0.5 size-4" />
                  <span>
                    <span className="block font-medium">New service</span>
                    <span className="block text-xs text-muted-foreground">
                      From a repository, template, Docker image or files; added
                      to {group.name} when created
                    </span>
                  </span>
                </Link>
              </DropdownMenuItem>
              <DropdownMenuItem
                className="items-start"
                onSelect={() => {
                  if (group.repository) {
                    setImporting(true)
                  } else {
                    navigate(
                      groupConnectRepositoryHref(slug, { thenImport: true })
                    )
                  }
                }}
              >
                <FolderGit2 className="mt-0.5 size-4" />
                <span>
                  <span className="block font-medium">From the repository</span>
                  <span className="block text-xs text-muted-foreground">
                    {group.repository
                      ? `Apps in ${group.repository.repo_owner}/${group.repository.repo_name}`
                      : 'Connect a monorepo and add its apps'}
                  </span>
                </span>
              </DropdownMenuItem>
              <DropdownMenuItem
                className="items-start"
                onSelect={() => setAdding(true)}
              >
                <Rows3 className="mt-0.5 size-4" />
                <span>
                  <span className="block font-medium">Existing services</span>
                  <span className="block text-xs text-muted-foreground">
                    Move services you already deploy into this project
                  </span>
                </span>
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        }
      />

      <div className="space-y-8">
        <GroupRepositoryPanel
          group={group}
          onConnect={() => navigate(connectHref)}
          onImport={() => setImporting(true)}
        />
        <GroupServicesPanel
          group={group}
          onAddServices={() => setAdding(true)}
        />
      </div>

      <AddServicesDialog group={group} open={adding} onOpenChange={setAdding} />
      <ImportServicesDialog
        group={group}
        open={importOpen}
        onOpenChange={setImportOpen}
      />
    </PageContainer>
  )
}
