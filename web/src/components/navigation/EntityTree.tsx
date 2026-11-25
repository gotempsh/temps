// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import {
  ChevronRight,
  Folder,
  FolderOpen,
  Globe,
  Plus,
  Server,
  type LucideIcon,
} from 'lucide-react'
import type { ReactNode } from 'react'
import { Link, useLocation } from 'react-router'
import {
  adminListNodesOptions,
  getProjectsMonitorHealthOptions,
  getProjectsOptions,
  listDomainsOptions,
  listServicesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import { useProjectGroups } from '@/components/project-groups/useProjectGroups'
import { ServiceLogo } from '@/components/ui/service-logo'
import {
  SidebarGroup,
  SidebarMenu,
  SidebarMenuAction,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarMenuSkeleton,
  SidebarMenuSub,
  SidebarMenuSubButton,
  SidebarMenuSubItem,
  useSidebar,
} from '@/components/ui/sidebar'
import {
  capRows,
  databaseState,
  domainState,
  locateInTree,
  projectNodes,
  serverState,
  serviceState,
  worstState,
  type ProjectNode,
  type ServiceNode,
  type TreeBranchId,
  type TreeState,
} from '@/lib/entity-tree'
import { projectRoot } from '@/lib/project-navigation'
import { SERVER_ENTITIES, SERVER_HOME } from '@/lib/server-navigation'
import { groupHref } from '@/lib/terminology'
import { cn } from '@/lib/utils'
import { WORKER_NODES_URL } from '@/lib/worker-nodes'
import { useTreeFold } from './use-tree-fold'

const ACTIVE = 'bg-sidebar-accent text-sidebar-accent-foreground'
const QUERY = { staleTime: 60_000, retry: false } as const
/** Rows a branch lists before it points at its own page for the rest. */
const BRANCH_ROWS = 8
/** Projects and services listed; the Projects page has the rest. */
const PROJECT_ROWS = 20
/** Services a monitor answer is asked for in one request. */
const HEALTH_IDS = 100
/** With this few projects, each opens on its own: the whole tree fits. */
const OPEN_ALL_BELOW = 6

interface Branch {
  id: TreeBranchId
  title: string
  icon: LucideIcon
  url: string
  /** What to offer while the branch holds nothing. */
  add?: { label: string; href: string }
}

/** The tree's branches: projects, then the other things the user owns. */
const BRANCHES: Branch[] = [SERVER_HOME, ...SERVER_ENTITIES].map((area) => ({
  id: area.id as TreeBranchId,
  title: area.title,
  icon: area.icon,
  url: area.url,
  add: area.add,
}))

function branchOf(id: TreeBranchId): Branch {
  const found = BRANCHES.find((candidate) => candidate.id === id)
  if (!found) throw new Error(`The tree has no "${id}" branch`)
  return found
}

const TROUBLE_TEXT = {
  warn: 'text-amber-700 dark:text-amber-400',
  bad: 'text-red-700 dark:text-red-400',
} as const

/**
 * A row's state. Working and idle are a quiet dot, filled or hollow, so the
 * name keeps the row; trouble is said in a word, read and not guessed from
 * a colour.
 */
function RowState({ state }: { state: TreeState | undefined }) {
  if (!state) return null
  if (state.tone === 'ok' || state.tone === 'idle')
    return (
      <span
        title={state.label}
        className={cn(
          'ml-auto size-1.5 shrink-0 rounded-full',
          state.tone === 'ok'
            ? 'bg-emerald-500'
            : 'border border-muted-foreground/60'
        )}
      >
        <span className="sr-only">{state.label}</span>
      </span>
    )
  return (
    <span
      className={cn(
        'ml-auto shrink-0 text-xs font-medium',
        TROUBLE_TEXT[state.tone]
      )}
    >
      {state.label}
    </span>
  )
}

function Fold({
  name,
  open,
  onToggle,
  nested = false,
}: {
  name: string
  open: boolean
  onToggle: (open: boolean) => void
  nested?: boolean
}) {
  return (
    <SidebarMenuAction
      aria-expanded={open}
      aria-label={open ? `Hide ${name}` : `Show ${name}`}
      title={open ? 'Hide' : 'Show'}
      onClick={() => onToggle(!open)}
      className={cn(nested && 'top-1')}
    >
      <ChevronRight
        className={cn('transition-transform', open && 'rotate-90')}
      />
    </SidebarMenuAction>
  )
}

/** A row that stands for the rest of a list, or for one that is empty. */
function NoteRow({
  to,
  onNavigate,
  children,
}: {
  to: string
  onNavigate: () => void
  children: ReactNode
}) {
  return (
    <SidebarMenuSubItem>
      <SidebarMenuSubButton asChild className="text-muted-foreground">
        <Link to={to} onClick={onNavigate}>
          {children}
        </Link>
      </SidebarMenuSubButton>
    </SidebarMenuSubItem>
  )
}

interface Leaf {
  key: string
  name: string
  url: string
  icon: ReactNode
  state: TreeState | undefined
  /** The page shown is this entity's, or one below it. */
  inside: boolean
}

function LeafRow({
  leaf,
  pathname,
  onNavigate,
}: {
  leaf: Leaf
  pathname: string
  onNavigate: () => void
}) {
  return (
    <SidebarMenuSubItem>
      <SidebarMenuSubButton asChild isActive={leaf.inside}>
        <Link
          to={leaf.url}
          aria-current={
            pathname === leaf.url ? 'page' : leaf.inside ? 'true' : undefined
          }
          onClick={onNavigate}
          className={cn(leaf.inside && ACTIVE)}
        >
          {leaf.icon}
          <span className="min-w-0 flex-1 truncate">{leaf.name}</span>
          <RowState state={leaf.state} />
        </Link>
      </SidebarMenuSubButton>
    </SidebarMenuSubItem>
  )
}

/** The children of a branch, in whichever state their list is. */
function BranchRows({
  branch,
  query,
  rows,
  more,
  pathname,
  onNavigate,
}: {
  branch: Branch
  query: { isPending: boolean; isError: boolean; refetch: () => unknown }
  rows: Leaf[]
  /** How many more exist than are listed. */
  more: number
  pathname: string
  onNavigate: () => void
}) {
  if (query.isError)
    return (
      <SidebarMenuSubItem className="flex h-7 items-center gap-2 px-2 text-xs text-muted-foreground">
        <span className="min-w-0 flex-1 truncate">Could not be loaded</span>
        <button
          type="button"
          onClick={() => void query.refetch()}
          className="shrink-0 rounded-sm font-medium text-foreground underline-offset-2 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sidebar-ring"
        >
          Retry
        </button>
      </SidebarMenuSubItem>
    )
  if (query.isPending)
    return (
      <li aria-busy="true" aria-label={`Loading ${branch.title}`}>
        <SidebarMenuSkeleton className="h-7" />
        <SidebarMenuSkeleton className="h-7" />
      </li>
    )
  if (rows.length === 0)
    return branch.add ? (
      <NoteRow to={branch.add.href} onNavigate={onNavigate}>
        <Plus />
        <span>{branch.add.label}</span>
      </NoteRow>
    ) : null
  return (
    <>
      {rows.map((leaf) => (
        <LeafRow
          key={leaf.key}
          leaf={leaf}
          pathname={pathname}
          onNavigate={onNavigate}
        />
      ))}
      {more > 0 && (
        <NoteRow to={branch.url} onNavigate={onNavigate}>
          <span>{`${more} more`}</span>
        </NoteRow>
      )}
    </>
  )
}

/**
 * The console's one menu: a tree of what the user owns. Projects open to
 * their services; databases, domains and servers are listed under their
 * own branch. A row says how its entity is doing, so the tree answers "is
 * it working" before anything is opened. A folded branch asks the server
 * nothing.
 */
export function EntityTree() {
  const { pathname } = useLocation()
  const { isMinimal, isMobile, setOpenMobile } = useSidebar()
  const compact = isMinimal && !isMobile
  const closeOnMobile = () => {
    if (isMobile) setOpenMobile(false)
  }
  const location = locateInTree(pathname)
  const fold = useTreeFold()
  const isOpen = (id: TreeBranchId) =>
    !compact &&
    // Projects is what most visits are for, so it starts open.
    fold.isOpen(`tree:${id}`, id === 'projects' || location.branch === id)

  const groupList = useProjectGroups()
  const services = useQuery({
    ...getProjectsOptions({ query: { per_page: 100, page: 1 } }),
    ...QUERY,
    enabled: isOpen('projects'),
  })
  const databases = useQuery({
    ...listServicesOptions(),
    ...QUERY,
    enabled: isOpen('databases'),
  })
  const domains = useQuery({
    ...listDomainsOptions({ query: { page: 1, page_size: BRANCH_ROWS } }),
    ...QUERY,
    enabled: isOpen('domains'),
  })
  const servers = useQuery({
    ...adminListNodesOptions(),
    ...QUERY,
    enabled: isOpen('servers'),
  })

  const nodes = projectNodes(groupList.groups, services.data?.projects ?? [])
  const isCurrentNode = (node: ProjectNode) =>
    node.kind === 'project'
      ? node.slug === location.groupSlug ||
        node.services.some((service) => service.slug === location.serviceSlug)
      : node.service.slug === location.serviceSlug
  const projects = capRows(nodes, PROJECT_ROWS, isCurrentNode)
  const unlisted =
    projects.more +
    (groupList.truncated ? 1 : 0) +
    Math.max(
      0,
      (services.data?.total ?? 0) - (services.data?.projects.length ?? 0)
    )
  const healthIds = projects.shown
    .flatMap((node) =>
      node.kind === 'project'
        ? node.services.map((service) => service.id)
        : [node.service.id]
    )
    .slice(0, HEALTH_IDS)
  const health = useQuery({
    ...getProjectsMonitorHealthOptions({
      query: { project_ids: healthIds.join(',') },
    }),
    ...QUERY,
    refetchInterval: 60_000,
    enabled: isOpen('projects') && healthIds.length > 0,
  })
  const stateOf = (service: ServiceNode) =>
    serviceState(health.data?.projects[String(service.id)]?.status)

  if (compact)
    return (
      <SidebarGroup className="py-2">
        <SidebarMenu aria-label="Your server">
          {BRANCHES.map((branch) => (
            <SidebarMenuItem key={branch.id}>
              <SidebarMenuButton
                asChild
                tooltip={branch.title}
                className={cn(
                  'justify-center',
                  location.branch === branch.id && ACTIVE
                )}
              >
                <Link
                  to={branch.url}
                  aria-current={pathname === branch.url ? 'page' : undefined}
                >
                  <branch.icon />
                </Link>
              </SidebarMenuButton>
            </SidebarMenuItem>
          ))}
        </SidebarMenu>
      </SidebarGroup>
    )

  const serviceLeaf = (service: ServiceNode): Leaf => ({
    key: `service:${service.id}`,
    name: service.name,
    url: projectRoot(service.slug),
    icon: <ServiceIcon service={service} size="xxs" />,
    state: stateOf(service),
    inside: service.slug === location.serviceSlug,
  })

  const databaseRows = capRows(
    databases.data ?? [],
    BRANCH_ROWS,
    (database) => database.id === location.recordId
  )
  const domainRows = domains.data?.domains ?? []
  const serverRows = capRows(
    servers.data?.nodes ?? [],
    BRANCH_ROWS,
    (node) => node.id === location.recordId
  )
  const on = (branch: TreeBranchId, id: number) =>
    location.branch === branch && location.recordId === id

  const children: Record<TreeBranchId, ReactNode> = {
    projects: (
      <ProjectRows
        query={
          // Projects and the services in none are two lists; both must
          // answer before the branch can say it is empty.
          groupList.isError || groupList.isPending ? groupList : services
        }
        nodes={projects.shown}
        more={unlisted}
        openAll={nodes.length < OPEN_ALL_BELOW}
        location={location}
        pathname={pathname}
        fold={fold}
        leafOf={serviceLeaf}
        stateOf={stateOf}
        onNavigate={closeOnMobile}
      />
    ),
    databases: (
      <BranchRows
        branch={branchOf('databases')}
        query={databases}
        more={databaseRows.more}
        pathname={pathname}
        onNavigate={closeOnMobile}
        rows={databaseRows.shown.map((database) => ({
          key: `database:${database.id}`,
          name: database.name,
          url: `/storage/${database.id}`,
          icon: (
            <ServiceLogo
              service={database.service_type}
              className="size-4 shrink-0"
            />
          ),
          state: databaseState(database.status),
          inside: on('databases', database.id),
        }))}
      />
    ),
    domains: (
      <BranchRows
        branch={branchOf('domains')}
        query={domains}
        more={Math.max(0, (domains.data?.total ?? 0) - domainRows.length)}
        pathname={pathname}
        onNavigate={closeOnMobile}
        rows={domainRows.map((domain) => ({
          key: `domain:${domain.id}`,
          name: domain.domain,
          url: `/domains/${domain.id}`,
          icon: <Globe />,
          state: domainState(domain.status),
          inside: on('domains', domain.id),
        }))}
      />
    ),
    servers: (
      <BranchRows
        branch={branchOf('servers')}
        query={servers}
        more={serverRows.more}
        pathname={pathname}
        onNavigate={closeOnMobile}
        rows={serverRows.shown.map((node) => ({
          key: `server:${node.id}`,
          name: node.name,
          url: `${WORKER_NODES_URL}/${node.id}`,
          icon: <Server />,
          state: serverState(node.status),
          inside: on('servers', node.id),
        }))}
      />
    ),
  }

  return (
    <SidebarGroup className="py-2">
      <SidebarMenu aria-label="Your server">
        {BRANCHES.map((branch) => {
          const open = isOpen(branch.id)
          const inside = location.branch === branch.id
          const exact = pathname === branch.url
          // Folded, the branch itself marks where the user is.
          const marked = exact || (inside && !open)
          return (
            <SidebarMenuItem key={branch.id} data-branch={branch.id}>
              <SidebarMenuButton
                asChild
                className={cn('justify-start pr-8', marked && ACTIVE)}
              >
                <Link
                  to={branch.url}
                  aria-current={exact ? 'page' : undefined}
                  onClick={closeOnMobile}
                >
                  <branch.icon />
                  <span>{branch.title}</span>
                </Link>
              </SidebarMenuButton>
              <Fold
                name={branch.title}
                open={open}
                onToggle={(next) => fold.setOpen(`tree:${branch.id}`, next)}
              />
              {open && (
                <SidebarMenuSub aria-label={branch.title}>
                  {children[branch.id]}
                </SidebarMenuSub>
              )}
            </SidebarMenuItem>
          )
        })}
      </SidebarMenu>
    </SidebarGroup>
  )
}

/** The rows under Projects: each project, foldable to its services. */
function ProjectRows({
  query,
  nodes,
  more,
  openAll,
  location,
  pathname,
  fold,
  leafOf,
  stateOf,
  onNavigate,
}: {
  query: { isPending: boolean; isError: boolean; refetch: () => unknown }
  nodes: ProjectNode[]
  more: number
  openAll: boolean
  location: ReturnType<typeof locateInTree>
  pathname: string
  fold: ReturnType<typeof useTreeFold>
  leafOf: (service: ServiceNode) => Leaf
  stateOf: (service: ServiceNode) => TreeState | undefined
  onNavigate: () => void
}) {
  const branch = branchOf('projects')
  if (query.isError || query.isPending || nodes.length === 0)
    return (
      <BranchRows
        branch={branch}
        query={query}
        rows={[]}
        more={0}
        pathname={pathname}
        onNavigate={onNavigate}
      />
    )
  return (
    <>
      {nodes.map((node) => {
        if (node.kind === 'service')
          return (
            <LeafRow
              key={node.key}
              leaf={leafOf(node.service)}
              pathname={pathname}
              onNavigate={onNavigate}
            />
          )
        const url = groupHref(node.slug)
        const holdsCurrent = node.services.some(
          (service) => service.slug === location.serviceSlug
        )
        const inside = node.slug === location.groupSlug || holdsCurrent
        const open = fold.isOpen(`tree:${node.key}`, inside || openAll)
        // Its own page, or folded over the service shown.
        const marked =
          node.slug === location.groupSlug || (holdsCurrent && !open)
        const services = capRows(
          node.services,
          PROJECT_ROWS,
          (service) => service.slug === location.serviceSlug
        )
        return (
          <SidebarMenuSubItem
            key={node.key}
            className="relative"
            data-project={node.slug}
          >
            <SidebarMenuSubButton asChild isActive={marked}>
              <Link
                to={url}
                aria-current={
                  pathname === url ? 'page' : marked ? 'true' : undefined
                }
                onClick={onNavigate}
                className={cn('pr-7', marked && ACTIVE)}
              >
                {open ? <FolderOpen /> : <Folder />}
                <span className="min-w-0 flex-1 truncate">{node.name}</span>
                {/* Open, each service says its own. */}
                {!open && (
                  <RowState state={worstState(node.services.map(stateOf))} />
                )}
              </Link>
            </SidebarMenuSubButton>
            <Fold
              nested
              name={node.name}
              open={open}
              onToggle={(next) => fold.setOpen(`tree:${node.key}`, next)}
            />
            {open && (
              <SidebarMenuSub
                aria-label={`${node.name} services`}
                // Flush right, so states line up down the whole tree.
                className="ml-2 mr-0 pl-2 pr-0"
              >
                {services.shown.length === 0 ? (
                  <NoteRow to={url} onNavigate={onNavigate}>
                    <span>No service yet</span>
                  </NoteRow>
                ) : (
                  services.shown.map((service) => (
                    <LeafRow
                      key={service.id}
                      leaf={leafOf(service)}
                      pathname={pathname}
                      onNavigate={onNavigate}
                    />
                  ))
                )}
                {services.more > 0 && (
                  <NoteRow to={url} onNavigate={onNavigate}>
                    <span>{`${services.more} more`}</span>
                  </NoteRow>
                )}
              </SidebarMenuSub>
            )}
          </SidebarMenuSubItem>
        )
      })}
      {more > 0 && (
        <NoteRow to={branch.url} onNavigate={onNavigate}>
          <span>More projects</span>
        </NoteRow>
      )}
    </>
  )
}
