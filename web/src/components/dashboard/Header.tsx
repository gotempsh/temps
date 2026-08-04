import {
  getLastDeploymentOptions,
  getProjectsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { AiAssistantButton } from '@/components/ai/AiAssistantButton'
import { BackupAlertsButton } from '@/components/dashboard/BackupAlertsButton'
import { DropButton } from '@/components/dashboard/DropButton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useConsoleExtensions } from '@temps-sdk/console-kit'
import { useQuery } from '@tanstack/react-query'
import { Check, ChevronsUpDown, Plus } from 'lucide-react'
import React, { useMemo, useState } from 'react'
import { Link, useLocation, useNavigate } from 'react-router'
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from '../ui/breadcrumb'
import { Avatar, AvatarFallback, AvatarImage } from '../ui/avatar'
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
} from '../ui/command'
import { Popover, PopoverContent, PopoverTrigger } from '../ui/popover'
import { Separator } from '../ui/separator'
import { SidebarTrigger } from '../ui/sidebar'

function ProjectRowIcon({
  projectId,
  name,
}: {
  projectId: number
  name: string
}) {
  const { data: lastDeployment } = useQuery({
    ...getLastDeploymentOptions({ path: { id: projectId } }),
    enabled: !!projectId,
  })
  const screenshot = lastDeployment?.screenshot_location
  if (screenshot) {
    const src = `/api/files${
      screenshot.startsWith('/') ? screenshot : '/' + screenshot
    }`
    return (
      <div className="size-5 shrink-0 overflow-hidden rounded-sm border bg-muted/30">
        <img
          src={src}
          alt={`${name} preview`}
          className="h-full w-full object-cover object-top"
        />
      </div>
    )
  }
  return (
    <Avatar className="size-5 rounded-sm">
      <AvatarImage src={`/api/projects/${projectId}/favicon`} />
      <AvatarFallback className="rounded-sm bg-muted text-[10px] font-medium text-muted-foreground">
        {name.slice(0, 1).toUpperCase()}
      </AvatarFallback>
    </Avatar>
  )
}

function ProjectSwitcher({
  currentSlug,
  label,
}: {
  currentSlug: string
  label: string
}) {
  const navigate = useNavigate()
  const [open, setOpen] = useState(false)
  const { data } = useQuery({
    ...getProjectsOptions({ query: { page: 1, per_page: 100 } }),
    enabled: open,
  })
  const projects = useMemo(
    () =>
      (data?.projects ?? [])
        .slice()
        .sort((a, b) =>
          a.name.localeCompare(b.name, undefined, { sensitivity: 'base' })
        ),
    [data?.projects]
  )

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          aria-label="Switch project"
          className="inline-flex min-w-0 max-w-full items-center gap-1.5 rounded-md px-1.5 py-0.5 text-sm font-normal text-foreground transition-colors hover:bg-accent"
        >
          <span className="max-w-[120px] truncate sm:max-w-[200px] lg:max-w-[280px]">
            {label}
          </span>
          <ChevronsUpDown className="size-3.5 shrink-0 text-muted-foreground" />
        </button>
      </PopoverTrigger>
      <PopoverContent
        className="w-[280px] p-0"
        align="start"
        side="bottom"
        sideOffset={6}
      >
        <Command>
          <CommandInput placeholder="Find Project…" />
          <CommandList>
            <CommandEmpty>No projects found.</CommandEmpty>
            <CommandGroup>
              {projects.map((p) => {
                const isCurrent = p.slug === currentSlug
                return (
                  <CommandItem
                    key={p.id}
                    value={`${p.name} ${p.slug}`}
                    onSelect={() => {
                      setOpen(false)
                      if (!isCurrent) {
                        navigate(`/projects/${p.slug}`)
                      }
                    }}
                  >
                    <ProjectRowIcon projectId={p.id} name={p.name} />
                    <span className="flex-1 truncate">{p.name}</span>
                    {isCurrent && (
                      <Check className="size-4 text-muted-foreground" />
                    )}
                  </CommandItem>
                )
              })}
            </CommandGroup>
            <CommandSeparator />
            <CommandGroup>
              <CommandItem
                onSelect={() => {
                  setOpen(false)
                  navigate('/projects/new')
                }}
              >
                <Plus className="size-4" />
                <span>Create Project</span>
              </CommandItem>
            </CommandGroup>
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  )
}

export function Header() {
  const { breadcrumbs } = useBreadcrumbs()
  const location = useLocation()
  // Extension-provided header actions (e.g. EE's SRE Copilot), rendered
  // leftmost in the top-right control cluster.
  const { headerActions } = useConsoleExtensions()

  const projectSlugMatch = location.pathname.match(/^\/projects\/([^/]+)/)
  const projectSlug =
    projectSlugMatch &&
    !['new', 'import-wizard', 'import'].includes(projectSlugMatch[1])
      ? projectSlugMatch[1]
      : null

  return (
    <header className="flex h-16 shrink-0 items-center gap-2 border-b px-4">
      <div className="flex w-full min-w-0 items-center justify-between gap-2">
        <div className="flex min-w-0 items-center overflow-hidden">
          <SidebarTrigger className="-ml-1 shrink-0" />
          <Separator orientation="vertical" className="mr-2 h-4 shrink-0" />
          <Breadcrumb className="min-w-0">
            <BreadcrumbList className="flex-nowrap min-w-0">
              {breadcrumbs.map((item, index) => {
                const isLast = index === breadcrumbs.length - 1
                const isProjectCrumb =
                  projectSlug !== null &&
                  (item.label === projectSlug ||
                    item.href === `/projects/${projectSlug}`)
                return (
                  <React.Fragment key={index}>
                    <BreadcrumbItem className="min-w-0">
                      {isProjectCrumb ? (
                        <ProjectSwitcher
                          currentSlug={projectSlug}
                          label={item.label}
                        />
                      ) : !isLast ? (
                        <BreadcrumbLink asChild href={item.href ?? '#'}>
                          <Link
                            to={item.href ?? '#'}
                            className="block max-w-[120px] truncate sm:max-w-[200px] lg:max-w-[260px]"
                          >
                            {item.label}
                          </Link>
                        </BreadcrumbLink>
                      ) : (
                        <BreadcrumbPage className="block max-w-[160px] truncate sm:max-w-[280px] lg:max-w-[360px]">
                          {item.label}
                        </BreadcrumbPage>
                      )}
                    </BreadcrumbItem>
                    {!isLast && <BreadcrumbSeparator />}
                  </React.Fragment>
                )
              })}
            </BreadcrumbList>
          </Breadcrumb>
        </div>
        <div className="ml-auto flex shrink-0 items-center space-x-2">
          {headerActions?.map((action) => (
            <React.Fragment key={action.id}>{action.element}</React.Fragment>
          ))}
          {/* The "+" quick-actions menu and the theme toggle used to live here.
              Every entry it held is now in the command palette (⌘K), and
              appearance moved into the account menu — so the header keeps only
              what you reach for mid-task: dropping files, the assistant, and
              alerts. */}
          <DropButton />
          <AiAssistantButton />
          <BackupAlertsButton />
        </div>
      </div>
    </header>
  )
}
