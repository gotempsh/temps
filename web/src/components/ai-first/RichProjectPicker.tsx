// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Check, ChevronsUpDown, PanelsTopLeft } from 'lucide-react'
import { useState } from 'react'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar'
import { Button } from '@/components/ui/button'
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from '@/components/ui/command'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { cn } from '@/lib/utils'
import {
  projectFaviconUrl,
  type ProjectPickerItem,
  type ProjectPickerTone,
} from './rich-project-picker'

export type {
  ProjectPickerItem,
  ProjectPickerTone,
} from './rich-project-picker'

type Props = {
  projects: ProjectPickerItem[]
  value: number | null
  onValueChange: (projectId: number) => void
  placeholder?: string
  disabled?: boolean
  ariaLabel: string
  className?: string
}

const STATUS_DOT: Record<ProjectPickerTone, string> = {
  healthy: 'bg-emerald-500',
  warning: 'bg-amber-500',
  down: 'bg-red-500',
  neutral: 'bg-zinc-400',
}

export function RichProjectPicker({
  projects,
  value,
  onValueChange,
  placeholder = 'Choose a project…',
  disabled,
  ariaLabel,
  className,
}: Props) {
  const [open, setOpen] = useState(false)
  const selected = projects.find((project) => project.id === value) ?? null

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          aria-expanded={open}
          aria-label={ariaLabel}
          className={cn(
            'h-auto min-h-10 min-w-0 flex-1 justify-between gap-2 p-2 font-normal',
            !selected && 'text-muted-foreground',
            className
          )}
          disabled={disabled}
          role="combobox"
          type="button"
          variant="outline"
        >
          {selected ? (
            <ProjectIdentity project={selected} />
          ) : (
            <span className="flex min-w-0 items-center gap-2 text-sm">
              <PanelsTopLeft className="size-4 shrink-0" />
              <span className="truncate">{placeholder}</span>
            </span>
          )}
          <ChevronsUpDown className="size-4 shrink-0 opacity-50" />
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="start"
        className="w-[min(calc(100vw-2rem),22rem)] min-w-[var(--radix-popover-trigger-width)] p-0"
      >
        <Command>
          <CommandInput name="project-filter" placeholder="Find a project…" />
          <CommandList className="max-h-80">
            <CommandEmpty>No projects found.</CommandEmpty>
            <CommandGroup heading="Projects">
              {projects.map((project) => (
                <CommandItem
                  className="items-start gap-2 p-2"
                  key={project.id}
                  onSelect={() => {
                    onValueChange(project.id)
                    setOpen(false)
                  }}
                  value={`${project.name} ${project.slug} ${project.status}`}
                >
                  <ProjectIdentity project={project} />
                  <Check
                    aria-hidden="true"
                    className={cn(
                      'size-4 shrink-0 self-center',
                      value === project.id ? 'opacity-100' : 'opacity-0'
                    )}
                  />
                </CommandItem>
              ))}
            </CommandGroup>
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  )
}

export function ProjectIdentity({ project }: { project: ProjectPickerItem }) {
  return (
    <span className="flex min-w-0 flex-1 items-center gap-2.5 text-left">
      <Avatar className="size-8 rounded-md border border-border bg-muted">
        <AvatarImage
          alt=""
          className="object-contain p-1.5"
          src={projectFaviconUrl(project.id)}
        />
        <AvatarFallback className="rounded-md text-xs font-medium text-muted-foreground">
          {project.name.slice(0, 1).toUpperCase()}
        </AvatarFallback>
      </Avatar>
      <span className="min-w-0 flex-1">
        <span className="flex min-w-0 items-center gap-2">
          <span className="truncate text-sm font-medium text-foreground">
            {project.name}
          </span>
          <span
            aria-hidden="true"
            className={cn(
              'size-2 shrink-0 rounded-full',
              STATUS_DOT[project.tone]
            )}
          />
        </span>
        <span className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
          <span className="truncate font-mono">{project.slug}</span>
          <span aria-hidden="true">·</span>
          <span className="shrink-0">{project.status}</span>
        </span>
      </span>
    </span>
  )
}
