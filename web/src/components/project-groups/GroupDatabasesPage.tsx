// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * A project's Databases page: the databases its apps use, managed the way a
 * single service's Databases page manages them (link, unlink, browse,
 * create), with which app uses each one and how.
 */

import { useState } from 'react'
import { Link, useNavigate } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ChevronRight,
  Database,
  HardDrive,
  Link2,
  MoreHorizontal,
  RotateCcw,
} from 'lucide-react'
import { toast } from 'sonner'
import type {
  DatabaseProvisioningMode,
  ExternalServiceInfo,
  ProjectGroupResponse,
} from '@/api/client'
import {
  linkServiceToProjectMutation,
  listProjectServicesQueryKey,
  listServicesOptions,
  unlinkServiceFromProjectMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { ResourceSparkline } from '@/components/project/ProjectStorage'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import { ServiceLinkRedeployPrompt } from '@/components/project/ServiceLinkRedeployPrompt'
import { CreateServiceButton } from '@/components/storage/CreateServiceButton'
import {
  DatabaseProvisioningDialog,
  type DatabaseProvisioningSelection,
} from '@/components/storage/DatabaseProvisioningDialog'
import { DatabaseTypeCards } from '@/components/storage/DatabaseTypeCards'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { SegmentedControl } from '@/components/ui/segmented-control'
import { ServiceLogo } from '@/components/ui/service-logo'
import { useKeyboardShortcut } from '@/hooks/useKeyboardShortcut'
import {
  CONTAINER_CPU_METRIC,
  containerCpuChartMetric,
  containerCpuHistoryMetric,
} from '@/lib/container-cpu'
import { formatCpuUsage } from '@/lib/cpu-format'
import { projectServiceResourcePath } from '@/lib/database-provisioning'
import { projectRoot } from '@/lib/project-navigation'
import type { GroupDatabase, GroupMember } from '@/lib/group-scoped-lists'
import { SERVER_WIDE_LISTS } from '@/lib/console-scope'
import { serviceBackupsHref } from '@/lib/service-backup-setup'
import { servicesWithSection } from '@/lib/service-kinds'
import type { ServiceLinkChange } from '@/lib/service-link-redeploy'
import { cn, formatBytes } from '@/lib/utils'
import {
  ScopedList,
  SectionNotApplicable,
  ServerWideButton,
} from './GroupScopedPages'
import { useGroupPageChrome } from './useGroupPageChrome'
import { groupServicesQuery, useGroupDatabases } from './useGroupResources'

/** Types that hold several databases, so linking asks which one to use. */
const PROVISIONED_TYPES = ['postgres', 'mariadb', 'mongodb']

const PROVISIONING_LABEL: Record<DatabaseProvisioningMode, string> = {
  custom: 'Custom database',
  project: 'Per service',
  project_environment: 'Per environment',
}

type LinkState = { database: ExternalServiceInfo; app: GroupMember }

export function GroupDatabasesPage({ group }: { group: ProjectGroupResponse }) {
  useGroupPageChrome(group, 'Databases')
  const queryClient = useQueryClient()
  const apps = servicesWithSection(group.projects, 'databases')
  const { databases, ...state } = useGroupDatabases(apps)

  // The app new and existing databases are linked to; the first by default.
  const [targetId, setTargetId] = useState<number | null>(null)
  const target = apps.find((app) => app.id === targetId) ?? apps[0]
  const [createOpen, setCreateOpen] = useState(false)
  const [pendingLink, setPendingLink] = useState<LinkState | null>(null)
  const [pendingUnlink, setPendingUnlink] = useState<LinkState | null>(null)
  const [change, setChange] = useState<{
    app: GroupMember
    change: ServiceLinkChange
  } | null>(null)

  useKeyboardShortcut({ key: 'n', callback: () => setCreateOpen(true) })

  // The redeploy prompt needs the app in full; read once a link changes.
  const fullApps = useQuery({
    ...groupServicesQuery(group.id),
    enabled: change != null,
  })
  const changedApp = fullApps.data?.find((app) => app.id === change?.app.id)

  const refresh = (app: GroupMember) =>
    queryClient.invalidateQueries({
      queryKey: listProjectServicesQueryKey({ path: { project_id: app.id } }),
    })

  const linkMutation = useMutation({
    ...linkServiceToProjectMutation(),
    meta: { errorTitle: 'Failed to link database to service' },
  })
  const unlinkMutation = useMutation({
    ...unlinkServiceFromProjectMutation(),
    meta: { errorTitle: 'Failed to unlink database from service' },
  })
  const busy = linkMutation.isPending || unlinkMutation.isPending

  const link = async (
    { database, app }: LinkState,
    selection?: DatabaseProvisioningSelection
  ) => {
    const promise = linkMutation.mutateAsync({
      path: { id: database.id },
      body: { project_id: app.id, ...selection },
    })
    toast.promise(promise, {
      loading: `Linking ${database.name} to ${app.name}...`,
      success: `${database.name} linked to ${app.name}`,
    })
    await promise
    setChange({
      app,
      change: {
        kind: 'linked',
        serviceName: database.name,
        appName: app.name,
      },
    })
    await refresh(app)
  }

  /** Links at once, or asks which database to use for a server type. */
  const requestLink = (next: LinkState) => {
    if (PROVISIONED_TYPES.includes(next.database.service_type)) {
      setPendingLink(next)
      return
    }
    void link(next).catch(() => {})
  }

  const unlink = async ({ database, app }: LinkState) => {
    const promise = unlinkMutation.mutateAsync({
      path: { id: database.id, project_id: app.id },
    })
    toast.promise(promise, {
      loading: `Unlinking ${database.name} from ${app.name}...`,
      success: `${database.name} unlinked from ${app.name}`,
    })
    await promise
    setChange({
      app,
      change: {
        kind: 'unlinked',
        serviceName: database.name,
        appName: app.name,
      },
    })
    await refresh(app)
  }

  const header = (
    <PageHeader
      title="Databases"
      description={
        apps.length > 0 && !state.loading ? (
          <span className="tabular-nums">
            {databases.length === 1
              ? "1 database used by this project's services."
              : `${databases.length} databases used by this project's services.`}
          </span>
        ) : undefined
      }
      actions={
        <div className="flex flex-wrap items-center gap-2">
          <ServerWideButton list={SERVER_WIDE_LISTS.databases} />
          <CreateServiceButton
            projectId={target?.id ?? null}
            label="Create database"
            open={createOpen}
            onOpenChange={setCreateOpen}
          />
        </div>
      }
    />
  )

  if (group.projects.length > 0 && apps.length === 0) {
    return (
      <PageContainer innerClassName="space-y-6">
        {header}
        <SectionNotApplicable group={group} page="databases" icon={Database} />
      </PageContainer>
    )
  }

  const targetPicker =
    apps.length > 1 && target ? (
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-sm text-muted-foreground">Add databases to</span>
        <SegmentedControl
          label="Service to add databases to"
          value={String(target.id)}
          onValueChange={(value) => setTargetId(Number(value))}
          options={apps.map((app) => ({
            value: String(app.id),
            label: app.name,
            media: <ServiceIcon service={app} size="xxs" />,
          }))}
        />
      </div>
    ) : null

  return (
    <PageContainer innerClassName="space-y-8">
      {header}

      {change && changedApp && (
        <ServiceLinkRedeployPrompt
          project={changedApp}
          change={change.change}
          onDismiss={() => setChange(null)}
        />
      )}

      <section aria-labelledby="project-linked-databases" className="space-y-2">
        <div className="flex items-baseline justify-between">
          <h2
            id="project-linked-databases"
            className="text-sm font-medium text-foreground"
          >
            Linked
          </h2>
          <span className="text-xs text-muted-foreground tabular-nums">
            {databases.length}
          </span>
        </div>
        <ScopedList
          state={state}
          isEmpty={databases.length === 0}
          noun="databases"
          icon={Database}
          emptyTitle="No databases linked yet"
          emptyDescription="Link an existing database or create one below."
        >
          <ul
            role="list"
            className="divide-y divide-border overflow-hidden rounded-lg border border-border"
          >
            {databases.map((database) => (
              <GroupDatabaseRow
                key={database.id}
                database={database}
                apps={apps}
                busy={busy}
                onLink={(app) =>
                  requestLink({ database: database.service, app })
                }
                onUnlink={(app) =>
                  setPendingUnlink({ database: database.service, app })
                }
              />
            ))}
          </ul>
        </ScopedList>
      </section>

      {targetPicker}

      {target && (
        <LinkExistingDatabase
          target={target}
          linked={databases.filter((db) =>
            db.usedBy.some((app) => app.id === target.id)
          )}
          busy={busy}
          onLink={(database) => requestLink({ database, app: target })}
        />
      )}

      <DatabaseTypeCards
        projectId={target?.id ?? null}
        heading={
          databases.length ? 'Create a new database' : 'Create a database'
        }
        description={
          target
            ? `Choose a database type to configure a new database linked to ${target.name}.`
            : 'Choose a database type to configure a new database.'
        }
      />

      <ConfirmDeleteDialog
        open={pendingUnlink !== null}
        onOpenChange={(open) => {
          if (!open) setPendingUnlink(null)
        }}
        verb="Unlink"
        resourceType="database service"
        resourceName={pendingUnlink?.database.name ?? ''}
        consequences={`${pendingUnlink?.app.slug ?? 'The service'} stops receiving its connection variables, and the running app loses access after its next deployment. The database, its data and its other links are kept.`}
        irreversible={false}
        onConfirm={() => {
          if (pendingUnlink) void unlink(pendingUnlink).catch(() => {})
        }}
      />
      {pendingLink && (
        <DatabaseProvisioningDialog
          key={`${pendingLink.database.id}-${pendingLink.app.id}`}
          open
          serviceName={pendingLink.database.name}
          projectSlug={pendingLink.app.slug}
          isPending={linkMutation.isPending}
          onOpenChange={(open) => {
            if (!open && !linkMutation.isPending) setPendingLink(null)
          }}
          onConfirm={async (selection) => {
            try {
              await link(pendingLink, selection)
              setPendingLink(null)
            } catch {
              /* Keep the selection for retry; the mutation toast explains the failure. */
            }
          }}
        />
      )}
    </PageContainer>
  )
}

/** Where an app's data lives in the database, as its service page shows it. */
function usageDetail(
  database: GroupDatabase,
  link: GroupDatabase['links'][number]['link'],
  app: GroupMember
) {
  const path = projectServiceResourcePath(
    database.type,
    app.slug,
    'production',
    link
  )
  return PROVISIONED_TYPES.includes(database.type)
    ? `${PROVISIONING_LABEL[link.database_provisioning_mode]} · ${path}`
    : path
}

function browseHref(database: GroupDatabase) {
  const first = database.links[0]
  if (!first) return `/storage/${database.id}/browse`
  const path = projectServiceResourcePath(
    database.type,
    first.member.slug,
    'production',
    first.link
  )
  return `/storage/${database.id}/browse?path=${encodeURIComponent(path)}`
}

/**
 * One database: what it is, how it is doing, and every app of the project
 * that uses it, with where that app's data lives. The row opens the
 * database; its menu links or unlinks it per app.
 */
export function GroupDatabaseRow({
  database,
  apps,
  busy,
  onLink,
  onUnlink,
}: {
  database: GroupDatabase
  /** The project's apps that can use a database. */
  apps: readonly GroupMember[]
  busy: boolean
  onLink: (app: GroupMember) => void
  onUnlink: (app: GroupMember) => void
}) {
  const navigate = useNavigate()
  const { service } = database
  const open = () => navigate(`/storage/${database.id}`)
  const cpuMetric = containerCpuChartMetric(
    CONTAINER_CPU_METRIC,
    service.topology === 'cluster'
  )
  const unlinkedApps = apps.filter(
    (app) => !database.usedBy.some((used) => used.id === app.id)
  )
  const details = [service.service_type, service.version]
    .filter(Boolean)
    .join(' ')

  return (
    <li
      role="button"
      tabIndex={0}
      aria-label={`${database.name}, used by ${database.usedBy.map((app) => app.name).join(', ')}`}
      onClick={open}
      onKeyDown={(e) => {
        if (e.target !== e.currentTarget) return
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault()
          open()
        }
      }}
      className={cn(
        'group flex items-start gap-4 px-3 py-3',
        'cursor-pointer transition-colors',
        'hover:bg-muted/60 focus-visible:bg-muted/60 focus-visible:outline-none'
      )}
    >
      <div className="mt-0.5 shrink-0">
        <ServiceLogo service={service.service_type} />
      </div>

      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <p className="truncate text-sm font-medium text-foreground">
            {database.name}
          </p>
          <Badge
            variant="outline"
            className={cn(
              'shrink-0',
              service.status === 'running'
                ? 'border-emerald-600/20 bg-emerald-500/10 text-emerald-700 dark:text-emerald-400'
                : 'text-muted-foreground'
            )}
          >
            {service.status}
          </Badge>
        </div>
        <p className="mt-0.5 truncate text-xs text-muted-foreground">
          {details}
          {service.topology === 'cluster' ? ' · cluster' : ''}
        </p>
        <ul aria-label="Used by" className="mt-2 space-y-1">
          {database.links.map(({ member, link }) => (
            <li
              key={member.id}
              className="flex min-w-0 items-center gap-2 text-xs"
            >
              <ServiceIcon service={member} size="xxs" />
              <Link
                to={projectRoot(member.slug)}
                className="shrink-0 font-medium text-foreground hover:underline"
                onClick={(event) => event.stopPropagation()}
              >
                {member.name}
              </Link>
              <span className="min-w-0 truncate text-muted-foreground">
                {usageDetail(database, link, member)}
              </span>
            </li>
          ))}
        </ul>
      </div>

      <ResourceSparkline
        serviceId={service.id}
        metric={cpuMetric}
        historyMetric={containerCpuHistoryMetric(
          CONTAINER_CPU_METRIC,
          cpuMetric
        )}
        label="CPU"
        format={(v) => formatCpuUsage(v, null)}
      />
      <ResourceSparkline
        serviceId={service.id}
        metric="container.memory_used_bytes"
        label="Mem"
        format={formatBytes}
      />

      <div
        className="flex items-center gap-1"
        onClick={(e) => e.stopPropagation()}
      >
        <Button
          variant="ghost"
          size="sm"
          className="h-8 gap-2 text-muted-foreground hover:text-foreground"
          onClick={() => navigate(browseHref(database))}
        >
          <Database className="size-3.5" />
          <span className="max-sm:sr-only">Browse</span>
        </Button>

        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              size="sm"
              className="relative size-8 p-0 text-muted-foreground hover:text-foreground"
              aria-label={`Actions for ${database.name}`}
            >
              <MoreHorizontal className="size-4" />
              <span
                aria-hidden="true"
                className="absolute top-1/2 left-1/2 size-[max(100%,3rem)] -translate-1/2 pointer-fine:hidden"
              />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-56">
            <DropdownMenuItem onSelect={open}>View details</DropdownMenuItem>
            <DropdownMenuItem onSelect={() => navigate(browseHref(database))}>
              Browse data
            </DropdownMenuItem>
            <DropdownMenuItem
              onSelect={() => navigate(serviceBackupsHref(database.id))}
            >
              <HardDrive className="size-3.5" />
              Backups
            </DropdownMenuItem>
            <DropdownMenuItem
              onSelect={() => navigate(`/storage/${database.id}/restore`)}
            >
              <RotateCcw className="size-3.5" />
              Restore…
            </DropdownMenuItem>
            {unlinkedApps.length > 0 && (
              <>
                <DropdownMenuSeparator />
                <DropdownMenuGroup>
                  <DropdownMenuLabel className="text-xs text-muted-foreground">
                    Link to
                  </DropdownMenuLabel>
                  {unlinkedApps.map((app) => (
                    <DropdownMenuItem
                      key={app.id}
                      disabled={busy}
                      onSelect={() => onLink(app)}
                    >
                      <ServiceIcon service={app} size="xxs" />
                      {app.name}
                    </DropdownMenuItem>
                  ))}
                </DropdownMenuGroup>
              </>
            )}
            <DropdownMenuSeparator />
            <DropdownMenuGroup>
              <DropdownMenuLabel className="text-xs text-muted-foreground">
                Unlink from
              </DropdownMenuLabel>
              {database.usedBy.map((app) => (
                <DropdownMenuItem
                  key={app.id}
                  disabled={busy}
                  className="text-destructive focus:bg-destructive/10 focus:text-destructive"
                  onSelect={() => onUnlink(app)}
                >
                  <ServiceIcon service={app} size="xxs" />
                  {app.name}
                </DropdownMenuItem>
              ))}
            </DropdownMenuGroup>
          </DropdownMenuContent>
        </DropdownMenu>

        <ChevronRight
          className="size-4 text-muted-foreground/60 transition-transform group-hover:translate-x-0.5 group-hover:text-muted-foreground"
          aria-hidden="true"
        />
      </div>
    </li>
  )
}

/**
 * The server's other databases, to link one to `target`. Loaded only when
 * asked for, as on a service's Databases page.
 */
function LinkExistingDatabase({
  target,
  linked,
  busy,
  onLink,
}: {
  target: GroupMember
  /** Databases `target` already uses, left out of the list. */
  linked: readonly GroupDatabase[]
  busy: boolean
  onLink: (database: ExternalServiceInfo) => void
}) {
  const [open, setOpen] = useState(false)
  const [search, setSearch] = useState('')
  const all = useQuery({
    ...listServicesOptions(),
    enabled: open,
    retry: false,
  })
  const term = search.trim().toLowerCase()
  const available = (all.data ?? []).filter(
    (database) =>
      !linked.some((used) => used.id === database.id) &&
      `${database.name} ${database.service_type}`.toLowerCase().includes(term)
  )

  return (
    <section aria-labelledby="link-existing-database" className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2
          id="link-existing-database"
          className="text-sm font-medium text-foreground"
        >
          Link an existing database
        </h2>
        {!open && (
          <Button
            variant="outline"
            size="sm"
            onClick={() => setOpen(true)}
            title="Choose from the databases on this server"
          >
            Choose database
          </Button>
        )}
      </div>
      {open &&
        (all.isLoading ? (
          <div
            role="status"
            aria-label="Loading databases on this server"
            className="h-24 animate-pulse rounded-lg bg-muted"
          />
        ) : all.isError ? (
          <Alert variant="destructive">
            <AlertTitle>
              The databases on this server could not be loaded
            </AlertTitle>
            <AlertDescription>
              The databases linked above are unaffected.
              <Button
                variant="outline"
                size="sm"
                className="ml-3"
                onClick={() => void all.refetch()}
              >
                Retry
              </Button>
            </AlertDescription>
          </Alert>
        ) : (
          <>
            <p className="text-xs text-muted-foreground">
              Databases on this server that {target.name} is not linked to.
              Linking makes its connection settings available from the next
              deployment.
            </p>
            <Input
              aria-label="Search existing databases"
              placeholder="Search by name or database type…"
              className="max-w-md"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
            />
            {available.length === 0 ? (
              <p className="py-4 text-sm text-muted-foreground">
                {search
                  ? 'No databases match your search.'
                  : `Every database on this server is already linked to ${target.name}.`}
              </p>
            ) : (
              <ul
                role="list"
                className="divide-y divide-border overflow-hidden rounded-lg border border-border"
              >
                {available.map((database) => (
                  <li
                    key={database.id}
                    className="flex items-center gap-4 px-3 py-3"
                  >
                    <ServiceLogo service={database.service_type} />
                    <div className="min-w-0 flex-1">
                      <p className="truncate text-sm font-medium">
                        {database.name}
                      </p>
                      <p className="truncate text-xs text-muted-foreground">
                        {[database.service_type, database.version]
                          .filter(Boolean)
                          .join(' ')}{' '}
                        · {database.status}
                      </p>
                    </div>
                    <Button
                      variant="outline"
                      size="sm"
                      className="h-8"
                      disabled={busy}
                      title={`Link ${database.name} to ${target.name}`}
                      onClick={() => onLink(database)}
                    >
                      <Link2 className="size-3.5" />
                      Link
                    </Button>
                  </li>
                ))}
              </ul>
            )}
          </>
        ))}
    </section>
  )
}
