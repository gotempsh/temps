// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Link } from 'react-router'
import { Boxes, FolderGit2, MoreHorizontal, Plus } from 'lucide-react'
import type {
  ProjectGroupMemberResponse,
  ProjectGroupResponse,
} from '@/api/client'
import { projectPresetLabel } from '@/components/dashboard/project-card-data'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { groupHref, serviceCount } from '@/lib/terminology'
import { AddServicesDialog } from './AddServicesDialog'
import { ExternalBadge } from '@/components/dashboard/ProjectCard'
import { ServicePresetIcon } from './ServicePresetIcon'
import { ServiceIcon } from '@/components/project/ServiceIcon'

/** Service rows shown before collapsing the rest into "+N more". */
const VISIBLE_SERVICES = 4
/** Preset logos stacked in the card's header. */
const STACKED_LOGOS = 3

/**
 * One project (group) on the projects list, sized like a service card: the
 * services' preset logos, a row per service, and the latest deployment. The
 * card opens the project's overview; each service row opens that service.
 */
export function ProjectGroupCard({ group }: { group: ProjectGroupResponse }) {
  const [adding, setAdding] = useState(false)
  const services = group.projects
  const shown = services.slice(0, VISIBLE_SERVICES)
  const hidden = services.length - shown.length
  const latest = latestDeployed(services)

  return (
    <div className="group relative flex min-h-44 flex-col rounded-xl border bg-card p-4 transition-colors focus-within:ring-2 focus-within:ring-ring hover:border-foreground/20 hover:bg-accent/70">
      <div className="flex min-w-0 items-start justify-between gap-3">
        <div className="flex min-w-0 flex-1 items-center gap-3">
          <PresetStack services={services} />
          <div className="min-w-0 flex-1">
            {/* Stretched link: the whole card opens the overview. */}
            <Link
              to={groupHref(group.slug)}
              className="block truncate font-semibold outline-none after:absolute after:inset-0 after:rounded-xl group-hover:underline"
            >
              {group.name}
            </Link>
            <p className="truncate text-xs text-muted-foreground">
              {group.repository ? (
                <span className="inline-flex max-w-full items-center gap-1 font-mono">
                  <FolderGit2 className="size-3 shrink-0" aria-hidden="true" />
                  <span className="truncate">
                    {group.repository.repo_owner}/{group.repository.repo_name} ·{' '}
                    {group.repository.main_branch}
                  </span>
                </span>
              ) : (
                group.description ||
                `Project · ${serviceCount(services.length)}`
              )}
            </p>
          </div>
        </div>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              size="icon"
              className="relative z-10 size-7 shrink-0"
              aria-label={`Actions for ${group.name}`}
            >
              <MoreHorizontal className="size-4" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem onSelect={() => setAdding(true)}>
              <Plus className="size-4" />
              Add services
            </DropdownMenuItem>
            <DropdownMenuItem asChild>
              <Link to={`${groupHref(group.slug)}?tab=variables`}>
                Shared variables
              </Link>
            </DropdownMenuItem>
            <DropdownMenuItem asChild>
              <Link to={`${groupHref(group.slug)}?tab=settings`}>
                Rename or delete
              </Link>
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      {services.length === 0 ? (
        <div className="mt-4 flex items-center justify-between gap-3 rounded-lg border border-dashed px-3 py-2.5">
          <span className="text-sm text-muted-foreground">No services yet</span>
          <Button
            variant="outline"
            size="sm"
            className="relative z-10"
            onClick={() => setAdding(true)}
          >
            <Plus className="size-4" />
            Add services
          </Button>
        </div>
      ) : (
        <ul className="-mx-1.5 mt-3 space-y-0.5">
          {shown.map((service) => (
            <li key={service.id}>
              <Link
                to={`/projects/${service.slug}`}
                className="relative z-10 flex min-w-0 items-center gap-2.5 rounded-md px-1.5 py-1 text-sm hover:bg-foreground/[0.06] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <ServiceIcon service={service} size="xs" />
                <span className="truncate font-medium">{service.name}</span>
                {service.hosting === 'external' ? (
                  <span className="ml-auto">
                    <ExternalBadge />
                  </span>
                ) : (
                  <span className="ml-auto flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground">
                    <ServicePresetIcon
                      preset={service.preset}
                      className="size-4 rounded"
                    />
                    {projectPresetLabel(service.preset)}
                  </span>
                )}
              </Link>
            </li>
          ))}
          {hidden > 0 && (
            <li className="px-1.5 py-1 text-xs text-muted-foreground">
              +{hidden} more {hidden === 1 ? 'service' : 'services'}
            </li>
          )}
        </ul>
      )}

      <div className="mt-auto pt-3">
        <div className="border-t pt-3">
          <p className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
            Latest deployment
          </p>
          {latest?.last_deployment ? (
            <p className="mt-1 truncate text-xs text-muted-foreground">
              <span className="font-medium text-foreground">{latest.name}</span>
              {' · '}
              <TimeAgo date={latest.last_deployment} />
            </p>
          ) : (
            <p className="mt-1 text-xs text-muted-foreground">Not deployed</p>
          )}
        </div>
      </div>

      <AddServicesDialog group={group} open={adding} onOpenChange={setAdding} />
    </div>
  )
}

/** The group's "logo": its services' preset logos, overlapping. */
function PresetStack({ services }: { services: ProjectGroupMemberResponse[] }) {
  if (services.length === 0) {
    return (
      <span className="flex size-10 shrink-0 items-center justify-center rounded-md border bg-muted/55 text-muted-foreground">
        <Boxes className="size-4" />
      </span>
    )
  }
  // One logo per distinct preset: three Next.js services read as one stack.
  const presets = [...new Set(services.map((s) => s.preset))]
  const shown = presets.slice(0, STACKED_LOGOS)
  const extra = presets.length - shown.length
  return (
    <span
      className="flex shrink-0 items-center -space-x-2"
      title={presets.map(projectPresetLabel).join(', ')}
    >
      {shown.map((preset) => (
        <ServicePresetIcon
          key={preset}
          preset={preset}
          className="size-8 rounded-lg ring-2 ring-card"
        />
      ))}
      {extra > 0 && (
        <span className="flex size-8 items-center justify-center rounded-lg border bg-background text-[10px] font-medium text-muted-foreground ring-2 ring-card">
          +{extra}
        </span>
      )}
    </span>
  )
}

function latestDeployed(services: ProjectGroupMemberResponse[]) {
  return services.reduce<ProjectGroupMemberResponse | undefined>((best, s) => {
    if (!s.last_deployment) return best
    if (!best?.last_deployment) return s
    return Date.parse(s.last_deployment) > Date.parse(best.last_deployment)
      ? s
      : best
  }, undefined)
}
