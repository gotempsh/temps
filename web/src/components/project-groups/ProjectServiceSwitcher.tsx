// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import { useLocation, useNavigate } from 'react-router'
import { ArrowRight, Check, ChevronsUpDown, Plus } from 'lucide-react'
import type { ProjectGroupResponse, ProjectResponse } from '@/api/client'
import { projectPresetLabel } from '@/components/dashboard/project-card-data'
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
} from '@/components/ui/command'
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover'
import { groupHref, serviceCount } from '@/lib/terminology'
import { AddServicesDialog } from './AddServicesDialog'
import { ServicePresetIcon } from './ServicePresetIcon'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import { siblingServicePath } from './sibling-service-path'

/** Services shown before the list gets a filter box. */
const FILTER_THRESHOLD = 7

/**
 * A grouped service's name as a switcher between the services of its
 * project. Switching keeps the section being viewed, so comparing, say, two
 * services' variables is one click.
 */
export function ProjectServiceSwitcher({
  project,
  group,
  children,
}: {
  project: ProjectResponse
  group: ProjectGroupResponse
  /** The trigger's label: the service's name as the page shows it. */
  children: ReactNode
}) {
  const navigate = useNavigate()
  const { pathname } = useLocation()
  const [open, setOpen] = useState(false)
  const [adding, setAdding] = useState(false)
  const services = group.projects

  const go = (to: string) => {
    setOpen(false)
    navigate(to)
  }

  return (
    <>
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <button
            type="button"
            aria-label={`${project.name}: switch to another service in ${group.name}`}
            title={`Switch between the ${serviceCount(services.length)} in ${group.name}`}
            className="inline-flex min-w-0 max-w-full items-center gap-1 rounded-md px-1 transition-colors hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring data-[state=open]:bg-muted"
          >
            <span className="truncate">{children}</span>
            <ChevronsUpDown className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
          </button>
        </PopoverTrigger>
        <PopoverContent className="w-[300px] p-0" align="start" sideOffset={6}>
          <Command>
            {services.length >= FILTER_THRESHOLD && (
              <CommandInput placeholder={`Find a service in ${group.name}…`} />
            )}
            <CommandList>
              <CommandEmpty>No services found.</CommandEmpty>
              <CommandGroup heading={`Services in ${group.name}`}>
                {services.map((service) => {
                  const isCurrent = service.id === project.id
                  return (
                    <CommandItem
                      key={service.id}
                      value={`${service.name} ${service.slug}`}
                      onSelect={() =>
                        isCurrent
                          ? setOpen(false)
                          : go(siblingServicePath(pathname, project.slug, service.slug))
                      }
                    >
                      <ServiceIcon service={service} size="xs" />
                      <span className="min-w-0 flex-1 truncate">{service.name}</span>
                      <span className="flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground">
                        <ServicePresetIcon preset={service.preset} className="size-4 rounded" />
                        {projectPresetLabel(service.preset)}
                      </span>
                      <Check
                        className={isCurrent ? 'size-4' : 'size-4 invisible'}
                        aria-hidden="true"
                      />
                    </CommandItem>
                  )
                })}
              </CommandGroup>
              <CommandSeparator />
              <CommandGroup>
                <CommandItem value="project overview" onSelect={() => go(groupHref(group.slug))}>
                  <ArrowRight className="size-4" />
                  <span>{group.name} overview</span>
                </CommandItem>
                <CommandItem
                  value="add services"
                  onSelect={() => {
                    setOpen(false)
                    setAdding(true)
                  }}
                >
                  <Plus className="size-4" />
                  <span>Add services…</span>
                </CommandItem>
              </CommandGroup>
            </CommandList>
          </Command>
        </PopoverContent>
      </Popover>
      <AddServicesDialog group={group} open={adding} onOpenChange={setAdding} />
    </>
  )
}
