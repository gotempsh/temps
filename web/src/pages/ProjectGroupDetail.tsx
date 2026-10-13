// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import { Link, useParams, useSearchParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import {
  ChevronDown,
  FolderGit2,
  Plus,
  RefreshCw,
  Rocket,
  Rows3,
} from 'lucide-react'
import { getProjectGroupBySlugOptions } from '@/api/client/@tanstack/react-query.gen'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { extractProblemDetails, getErrorMessage } from '@/utils/errorHandling'
import { groupHref, serviceCount } from '@/lib/terminology'
import { AddServicesDialog } from '@/components/project-groups/AddServicesDialog'
import { GroupServicesPanel } from '@/components/project-groups/GroupServicesPanel'
import { GroupVariablesPanel } from '@/components/project-groups/GroupVariablesPanel'
import { GroupSettingsPanel } from '@/components/project-groups/GroupSettingsPanel'
import { GroupRepositoryPanel } from '@/components/project-groups/GroupRepositoryPanel'
import { ConnectRepositoryDialog } from '@/components/project-groups/ConnectRepositoryDialog'
import { ImportServicesDialog } from '@/components/project-groups/ImportServicesDialog'
import { newServiceInGroupHref } from '@/components/project-groups/useJoinGroupAfterCreate'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'

const TABS = ['overview', 'variables', 'settings'] as const
type Tab = (typeof TABS)[number]

/** Overview of a project (group): its services, databases and shared config. */
export function ProjectGroupDetail() {
  const { slug = '' } = useParams()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [searchParams, setSearchParams] = useSearchParams()
  const requested = searchParams.get('tab')
  const tab: Tab = TABS.includes(requested as Tab)
    ? (requested as Tab)
    : 'overview'
  const [adding, setAdding] = useState(false)
  const [connecting, setConnecting] = useState(false)
  const [importing, setImporting] = useState(false)
  // "From the repository" without a repository connects one first, then
  // continues into the import.
  const [importAfterConnect, setImportAfterConnect] = useState(false)

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

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: group?.name ?? slug, href: groupHref(slug) },
    ])
  }, [group?.name, setBreadcrumbs, slug])

  usePageTitle(group?.name ?? 'Project')

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
                    setImportAfterConnect(true)
                    setConnecting(true)
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

      <Tabs
        value={tab}
        onValueChange={(value) =>
          setSearchParams({ tab: value }, { replace: true })
        }
      >
        <TabsList>
          <TabsTrigger value="overview">Overview</TabsTrigger>
          <TabsTrigger value="variables">Shared variables</TabsTrigger>
          <TabsTrigger value="settings">Settings</TabsTrigger>
        </TabsList>
        <TabsContent value="overview" className="mt-6 space-y-8">
          <GroupRepositoryPanel
            group={group}
            onConnect={() => setConnecting(true)}
            onImport={() => setImporting(true)}
          />
          <GroupServicesPanel
            group={group}
            onAddServices={() => setAdding(true)}
          />
        </TabsContent>
        <TabsContent value="variables" className="mt-6">
          <GroupVariablesPanel group={group} />
        </TabsContent>
        <TabsContent value="settings" className="mt-6">
          <GroupSettingsPanel
            group={group}
            onConnectRepository={() => setConnecting(true)}
          />
        </TabsContent>
      </Tabs>

      <AddServicesDialog group={group} open={adding} onOpenChange={setAdding} />
      <ConnectRepositoryDialog
        group={group}
        open={connecting}
        onOpenChange={(open) => {
          setConnecting(open)
          if (!open) setImportAfterConnect(false)
        }}
        onConnected={() => {
          if (importAfterConnect) setImporting(true)
          setImportAfterConnect(false)
        }}
      />
      <ImportServicesDialog
        group={group}
        open={importing}
        onOpenChange={setImporting}
      />
    </PageContainer>
  )
}
